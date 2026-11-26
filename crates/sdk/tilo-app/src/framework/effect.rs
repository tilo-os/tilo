//! Effects and jobs: work with no document delta, invoked through the same
//! API as actions, answered by the app.
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
//! [`ResourceEnv::outbox`](super::ResourceEnv::outbox): the framework runs the app's
//! handler in the live app on the device that owns the entry and records how
//! it settled.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::{Rc, Weak};

mod calls;
mod causal;
mod file;
mod job;
mod window;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use ui_core::automerge::transaction::Transactable;

use tilo_actions::{
    ActionFailure, ActionOutput, ActionSpec, Availability, InvocationId, InvocationUpdate,
};
use tilo_resource::{Effect, Id, Outbox, Path, Resource, Settled};
use tilo_ui_base::host_operation::{ResourceOperation, ResourceOperationStart};
use ui_core::WakeHandle;

use super::dispatch::Dispatcher;
use super::live::Live;

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
    pub identity: Option<ui_core::resource::DocumentId>,
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
    /// ([`ResourceEnv::outbox`](super::ResourceEnv::outbox)). [`Self::value`] with the
    /// result alone settles it done.
    pub fn settled<O: serde::Serialize>(settled: &Settled<O>) -> Self {
        Self::value(settled)
    }

    /// Finished, answering with the content of `hash` in this device's
    /// content store (an effect declared `content`), which another app's
    /// content-collecting operation takes.
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
pub struct Progress {
    events: Sender<JobEvent>,
    cancelled: Arc<AtomicBool>,
    wake: WakeHandle,
}

// Written out rather than derived: a derived `clone` is inlined into every
// app that clones a `Progress`, and this one is compiled once, in the
// runtime.
impl Clone for Progress {
    fn clone(&self) -> Self {
        Self {
            events: self.events.clone(),
            cancelled: Arc::clone(&self.cancelled),
            wake: self.wake.clone(),
        }
    }
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
        self.with_effect(|| Ok(())).is_err()
    }

    /// Share the actual job's cancellation flag with blocking native I/O.
    /// Treat the returned flag as read-only.
    #[must_use]
    pub fn cancellation_signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// Run a unit of external work unless the job was cancelled. Blocking
    /// workers use this between units. It grants no access to the canonical
    /// document.
    ///
    /// # Errors
    ///
    /// Returns an error if the job was cancelled or the unit of work returned
    /// an error.
    pub fn with_effect<T>(&self, effect: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("the effect was cancelled".into());
        }
        effect()
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

use job::{JobCore, Tracking, track_for};

/// What [`JobCore::next`] found.
enum Next {
    Progress(Polled),
    /// The job failed: it is over.
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

/// Start tracking a job in `started` for `owner`, with the completion
/// `then`; the [`Progress`] its work reports through. A job already
/// started there is stopped: an effect runs one job at a time.
fn track_job(
    started: &mut Option<Job>,
    wake: &WakeHandle,
    then: Option<Box<dyn Any>>,
    owner: std::any::TypeId,
    cancel_by: Option<Arc<AtomicBool>>,
) -> Progress {
    let (events, receiver) = channel();
    let signals = job::Signals::own();
    let progress = Progress {
        events,
        cancelled: Arc::clone(&signals.cancelled),
        wake: wake.clone(),
    };
    if let Some(previous) = started.replace(Job {
        core: JobCore::of(
            receiver,
            job::Signals {
                cancel_by,
                ..signals
            },
        ),
        then,
        owner,
    }) {
        tracing::warn!("an effect started a second job; the first one is stopped");
        previous.stop();
    }
    progress
}

/// How an outbox entry, or work a chain owes, starts: `work` given the value
/// in an effect context of its invocation.
fn start_entry<R: Resource + 'static, T: 'static>(
    starting: EffectEnv<R>,
    mut work: impl FnMut(&Id, T, &mut EffectCx<R>) -> EffectAnswer + 'static,
) -> StartEntry {
    Box::new(move |id, value, invocation| {
        let Ok(value) = value.downcast::<T>() else {
            return Err("an outbox entry is of another type".into());
        };
        let mut cx = EffectCx::invoked(starting.clone(), Answering::of(invocation));
        let mut value = Some(*value);
        let answer = run_under(invocation, &mut || match value.take() {
            Some(value) => work(id, value, &mut cx),
            None => Err("an outbox entry ran twice".into()),
        });
        Ok((answer, cx.inv.started.take()))
    })
}

/// Run `run`, a handler or a completion, for `invocation`.
fn run_under(
    invocation: Option<InvocationId>,
    run: &mut dyn FnMut() -> EffectAnswer,
) -> EffectAnswer {
    let _acting = ActingFor::enter(invocation);
    run()
}

/// A job's completion run: the completion, what the job finished with and
/// the invocation it answers; what it answered and the job it
/// chained, if any.
type Complete<'a> = dyn FnMut(
        Box<dyn Any>,
        Box<dyn Any + Send>,
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
    invocation: Option<InvocationId>,
) -> Result<(EffectAnswer, Option<Job>), String> {
    let Ok(then) = then.downcast::<Then<R, H>>() else {
        return Err("a job's completion is of another type".into());
    };
    let mut cx = EffectCx::<R, H>::invoked(env.clone(), Answering::of(invocation));
    let mut then = Some(*then);
    let mut value = Some(value);
    let answer = run_under(invocation, &mut || match (then.take(), value.take()) {
        (Some(then), Some(value)) => then(host, value, &mut cx),
        _ => Err("a job's completion ran twice".into()),
    });
    Ok((answer, cx.inv.started.take()))
}

/// What every effect context of one resource shares.
pub(crate) struct EffectEnv<R: Resource> {
    pub(crate) dispatch: Dispatcher<R>,
    pub(crate) live: Live<R>,
    pub(crate) wake: WakeHandle,
    /// Where the window says which file it shows, when it has one: a document saved
    /// somewhere new says so here.
    pub(crate) shows: Option<ui_core::application_view::WindowShows>,
    /// The media types of the files the app opens: a save to a file it would open is
    /// its document, one to any other file is a copy.
    pub(crate) opens: &'static [&'static str],
    scope: Option<std::rc::Weak<super::surface::SurfaceScope>>,
}

impl<R: Resource> Clone for EffectEnv<R> {
    fn clone(&self) -> Self {
        Self {
            dispatch: self.dispatch.clone(),
            live: self.live.clone(),
            wake: self.wake.clone(),
            shows: self.shows.clone(),
            opens: self.opens,
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
}

/// Tie `progress` to the surface `scope` when there is one: a job of a
/// retired surface is cancelled.
#[inline(never)]
fn track_in(scope: Option<&std::rc::Weak<super::surface::SurfaceScope>>, progress: &Progress) {
    if let Some(scope) = scope {
        if let Some(scope) = scope.upgrade() {
            scope.track(&progress.cancelled);
        } else {
            progress.cancelled.store(true, Ordering::Release);
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
    /// Everything else, which does not name the resource type.
    inv: Answering,
    /// What its completions get `&mut` access to.
    host: std::marker::PhantomData<fn(&mut H)>,
}

/// What an effect context holds besides its resource: the invocation it
/// answers and what came with it. Not generic, so making, moving and
/// dropping it is compiled once rather than in every app's contexts.
struct Answering {
    invocation: Option<InvocationId>,
    started: Option<Job>,
    /// The resource's document while a surface's handler answers an
    /// invocation, lent to it for reading.
    document: Option<Rc<RefCell<tilo_resource::Doc>>>,
    /// The lent document's heads, taken when it was lent.
    heads: Vec<ui_core::automerge::ChangeHash>,
    /// The invocation the host delivered for someone that this answers:
    /// what another resource's action this effect calls continues
    /// ([`EffectCx::call`]). `None` for the person at this device acting
    /// here, and for a view's own job.
    delivered: Option<InvocationId>,
    target: Option<tilo_ui_base::host_operation::OperationTarget>,
    /// Where the invoker asked the effect to act (`#[param(place)]`).
    place: Option<tilo_resource::Place>,
    /// The caller's cancellation signal, which a job this effect leaves to
    /// answer later is stopped by when the caller gives up waiting
    /// ([`EffectCx::cancel_by`]).
    cancel_by: Option<Arc<AtomicBool>>,
}

impl Answering {
    /// Answering `invocation`, with nothing else yet.
    #[inline(never)]
    fn of(invocation: Option<InvocationId>) -> Self {
        Self {
            invocation,
            started: None,
            document: None,
            heads: Vec::new(),
            delivered: None,
            target: None,
            place: None,
            cancel_by: None,
        }
    }
}

/// The place `effect` was told to act at: its argument declared
/// `#[param(place)]`, when given.
fn place_of<E: Effect>(
    decl: &tilo_resource::effect::EffectDecl,
    effect: &E,
) -> Option<tilo_resource::Place> {
    let param = place_param(decl)?;
    place_from(effect.arguments().remove(param)?)
}

/// Where an effect that shows a row ([`tilo_actions::SCREEN_PLACE_TAG`]) is
/// answered when someone other than this window's person, `me`, invoked it:
/// in the invoker's own view, at the row the effect is offered on. `None`
/// for any other effect, for this window's person, whose screen it moves,
/// and for an operation resolved against no resource.
fn shown_elsewhere(
    decl: &tilo_resource::effect::EffectDecl,
    row: Option<&str>,
    me: Option<&tilo_ui_base::permissions::MemberId>,
) -> Option<(tilo_actions::PresenceId, tilo_resource::Place)> {
    if !decl.tags.contains(&tilo_actions::SCREEN_PLACE_TAG) {
        return None;
    }
    let invoker = ui_core::invocation_scope::current_invoker()?;
    if invoker.acting.by.member_id() == me {
        return None;
    }
    let view = invoker.view()?;
    Some((view, tilo_resource::Place::Item(row?.to_owned())))
}

/// The name of the parameter `decl` declares `#[param(place)]`, if any.
#[inline(never)]
fn place_param(decl: &tilo_resource::effect::EffectDecl) -> Option<&str> {
    decl.params
        .iter()
        .find(|param| param.is_place())
        .map(|param| param.name.as_ref())
}

/// A place argument as given, read once in the runtime rather than in
/// every app.
#[inline(never)]
fn place_from(value: serde_json::Value) -> Option<tilo_resource::Place> {
    tilo_failure::ignored!(
        "an argument that is not a place names no place, and the effect runs without one",
        serde_json::from_value(value).ok()
    )
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

/// Run an effect's future on the app runtime, stopped once cancelled, and
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
/// unless cancelled, and deliver what it finished with ([`spawn_future`]).
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
            inv: Answering::of(invocation),
            host: std::marker::PhantomData,
        }
    }

    /// The context of the invocation `inv` describes.
    fn invoked(env: EffectEnv<R>, inv: Answering) -> Self {
        Self {
            env,
            inv,
            host: std::marker::PhantomData,
        }
    }

    /// Where the invoker asked this effect to act (its `#[param(place)]`
    /// argument): act there instead of at their caret. Once it succeeds
    /// for a presence, they land there ([`Live::place_of`]).
    pub fn place(&self) -> Option<&tilo_resource::Place> {
        self.inv.place.as_ref()
    }

    /// The resource's document as it is now, to read what a view answers
    /// with: the state at its heads, a selection's measurements, a picture
    /// of it. Only a surface's handler answering an invocation has it; a
    /// job's completion, which runs later, reads the live state instead.
    /// Changes go through the dispatcher, never here.
    pub fn document(&self) -> Option<std::cell::Ref<'_, tilo_resource::Doc>> {
        self.inv.document.as_ref().map(|document| document.borrow())
    }

    /// The heads of the document as it is now; empty without one.
    pub fn heads(&self) -> Vec<ui_core::automerge::ChangeHash> {
        self.inv.heads.clone()
    }

    /// The resource at `version`, as a caller names one: a change id from
    /// its history (or enough of one) or a checkpoint's name.
    pub fn version(&self, version: &str) -> Result<DocumentVersion, String> {
        let document = self
            .inv
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
    /// delivered it for someone, else the presence the view is for.
    /// `None` for a bot, an agent or an app acting, and in a past version.
    /// An effect that answers or changes what is someone's own ("my link")
    /// reads whose it is here, never from the view.
    pub fn person(&self) -> Option<String> {
        match ui_core::invocation_scope::current_invoker() {
            Some(invoker) => invoker.acting.by.member_id().cloned(),
            None => self.env.dispatch.me().cloned(),
        }
        .and_then(|member| member.person_id().map(str::to_owned))
    }

    /// Who the invocation this work continues was admitted for, acting for the person at this
    /// window when it is not that person: what a change it makes elsewhere names as its author.
    /// `None` for work no invocation started.
    pub fn invoker(&self) -> Option<tilo_actions::ActionInvoker> {
        let invoker = ui_core::invocation_scope::current_invoker()?;
        if invoker.acting.is_person() || !invoker.acting.acted_for.is_empty() {
            return Some(invoker);
        }
        let person = self
            .env
            .dispatch
            .me()
            .and_then(|me| me.person_id().map(str::to_owned));
        Some(match person {
            Some(person) => invoker.acting_for(person),
            None => invoker,
        })
    }

    /// The person's approve this invocation was sent again with: `Some` once they approved
    /// the call this effect refused for want of their yes
    /// ([`tilo_actions::ActionFailure::asks`]), and then it runs.
    pub fn approval(&self) -> Option<tilo_actions::remote::asked::Approval> {
        ui_core::invocation_scope::delivered_approval()
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
        self.inv.invocation
    }

    /// Submit the exact approved moderation operation through the record's host.
    /// Pass the returned Send future to the action's existing job.
    pub fn external_moderation(
        &self,
        operation: registry_types::ExternalModerationOperation,
    ) -> Result<ui_core::invocation_scope::ExternalModerationFuture, String> {
        let target = self
            .inv
            .target
            .as_ref()
            .ok_or("external moderation requires an exact dispatched record target")?;
        if target.route().app_id != self.env.dispatch.app_id() {
            return Err("external moderation dispatch app changed".into());
        }
        target.external_moderation(
            self.inv
                .invocation
                .ok_or("external moderation requires a dispatched invocation")?,
            operation,
        )
    }

    /// Wakes the app from another thread, to draw what a worker changed.
    pub fn waker(&self) -> WakeHandle {
        self.env.wake.clone()
    }

    fn track(&mut self, then: Option<Box<dyn Any>>) -> Progress {
        track_for(
            &mut Tracking {
                started: &mut self.inv.started,
                wake: &self.env.wake,
                scope: self.env.scope.as_ref(),
                cancel_by: self.inv.cancel_by.as_ref(),
                owner: std::any::TypeId::of::<Then<R, H>>(),
            },
            then,
        )
    }

    /// `then` as a job's completion, which continues the invocation this
    /// effect continues for whoever the host delivered it for.
    fn then<T: Send + 'static>(
        &self,
        then: impl FnOnce(&mut H, T, &mut Self) -> EffectAnswer + 'static,
    ) -> Box<dyn Any> {
        let delivered = self.inv.delivered;
        let then: Then<R, H> = Box::new(move |host, value, cx| match value.downcast::<T>() {
            Ok(value) => {
                cx.inv.delivered = cx.inv.delivered.or(delivered);
                then(host, *value, cx)
            }
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
        let progress = self.track(Some(self.then(then)));
        let future = work(progress.clone());
        let task = spawn_future(
            progress,
            Box::pin(async move { Box::new(future.await) as Box<dyn Any + Send> }),
        );
        if let Some(started) = self.inv.started.as_mut() {
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
        let progress = self.track(Some(self.then(then)));
        let task = spawn_work(
            progress,
            Box::new(move |progress| Box::new(work(progress)) as Box<dyn Any + Send>),
        );
        if let Some(started) = self.inv.started.as_mut() {
            started.core.task = Some(task);
        }
        Effected::Pending
    }

    /// Have the job this effect leaves to answer later stopped when `signal`
    /// goes off, as well as when the caller cancels it.
    ///
    /// An effect whose answer is read from a long-lived surface ([`Self::reply`])
    /// can lose its caller before the caller's own cancellation reaches it: the
    /// answer is kept by the surface rather than by a job of this invocation.
    /// A handler of such an effect passes the caller's cancellation signal
    /// ([`Progress::cancellation_signal`]) here, and the job that carries the
    /// answer stops with it: an operation nobody waits for must not run on.
    pub fn cancel_by(&mut self, signal: Arc<AtomicBool>) {
        self.inv.cancel_by = Some(signal);
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
    /// and preserves its structured failure. Cancellation is checked by the same job path as [`Self::spawn`].
    pub fn reply_then(
        &mut self,
        then: impl FnOnce(&mut H, ActionOutput, &mut Self) -> EffectAnswer + 'static,
    ) -> EffectReply {
        let progress = self.track(Some(self.then(then)));
        EffectReply {
            progress,
            completion: true,
        }
    }

    /// Answer once what this effect submitted through its
    /// [`dispatcher`](Self::dispatcher) landed: each submission goes through
    /// the API like any other, checked again for the effect's invoker, and
    /// `then` runs with the last one's [`Landed`](super::Landed) — what the API answered
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
            .inv
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
    /// wakes, so the sender must wake it, as the host's services do, or the
    /// app must be drawing frames, as it does while a render it pumps runs.
    pub fn wait_for<T: Send + 'static>(
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
        let progress = self.track(Some(self.then(then)));
        if let Some(started) = self.inv.started.as_mut() {
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
        let progress = self.track(Some(self.then(then)));
        if let Some(started) = self.inv.started.as_mut() {
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
/// surface's completion gets `&mut` the surface, and a window's the window
/// ([`ViewCx::window_jobs`](super::ViewCx::window_jobs)). Jobs run only in a
/// window someone uses: a past version and a capture start none.
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
            queue: JobQueue::shared(),
        }
    }

    /// Start the job `key` as `start` does it, through an [`EffectCx`] whose
    /// completions get `&mut H`: the surface itself for a surface's jobs
    /// (`jobs.start::<Self>(…)`), the window for a window's
    /// ([`ViewCx::window_jobs`](super::ViewCx::window_jobs)), `()` for a
    /// view's. Work it finishes at once needs no job; a failure is logged,
    /// as nobody waits for it.
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
            .started(key.into(), answer, cx.inv.started.take(), R::KIND);
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
            &mut |then, value, invocation| complete::<R, H>(&env, host, then, value, invocation),
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
    /// An empty queue, shared: made here once rather than in every app's
    /// [`Jobs`].
    #[inline(never)]
    fn shared() -> Rc<RefCell<Self>> {
        Rc::default()
    }

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

pub use tilo_resource::status::EffectStatus;

/// The specs last built for one vocabulary, as callers saw them then: the
/// locale and statuses they were built at, kept so a publication that finds
/// both unchanged copies them instead of building every declaration again.
pub(crate) type KeptSpecs = std::cell::RefCell<Option<(u64, Vec<EffectStatus>, Vec<ActionSpec>)>>;

/// The specs of `E` as callers see them at `statuses`, taken from `kept`
/// while the locale and the statuses are those it was built at.
pub(crate) fn kept_specs<E: Effect>(
    kept: &KeptSpecs,
    statuses: &[EffectStatus],
) -> Vec<ActionSpec> {
    kept_specs_of(kept, statuses, E::specs)
}

/// [`kept_specs`] of the effects whose specs `specs` builds: compiled once
/// rather than for every effect vocabulary.
#[inline(never)]
pub(crate) fn kept_specs_of(
    kept: &KeptSpecs,
    statuses: &[EffectStatus],
    specs: fn() -> Vec<ActionSpec>,
) -> Vec<ActionSpec> {
    let locale = tilo_ui_base::i18n::I18nManager::revision();
    if let Some((_, _, specs)) = kept
        .borrow()
        .as_ref()
        .filter(|(at, was, _)| *at == locale && was.as_slice() == statuses)
    {
        return specs.clone();
    }
    let specs = overlaid(specs(), statuses);
    *kept.borrow_mut() = Some((locale, statuses.to_vec(), specs.clone()));
    specs
}

/// `specs` with what `statuses` say of each laid over it, and unavailable
/// where they need what this host lacks: compiled once rather than for
/// every effect vocabulary.
#[inline(never)]
fn overlaid(specs: Vec<ActionSpec>, statuses: &[EffectStatus]) -> Vec<ActionSpec> {
    specs
        .into_iter()
        .map(|mut spec| {
            let id = spec.id.clone();
            for status in statuses
                .iter()
                .filter(|status| status.item().is_none() && status.id() == id)
            {
                status.overlay(&mut spec);
            }
            require_host(&mut spec, host_lacks);
            spec
        })
        .collect()
}

/// `spec` unavailable when one of its [`tilo_actions::REQUIRES_HOST_PREFIX`]
/// requirements is something `lacks` gives the host's reason for lacking.
fn require_host(spec: &mut ActionSpec, lacks: impl Fn(&str) -> Option<&'static str>) {
    if let Some(reason) = spec.contract.tags.iter().find_map(|tag| lacks(tag)) {
        spec.type_mut().contract.availability = Availability::Unavailable {
            code: tilo_actions::HOST_UNSUPPORTED_CODE.to_owned(),
            reason: reason.to_owned(),
        };
    }
}

/// Why this host cannot meet `tag`, when it is a host requirement it lacks.
fn host_lacks(tag: &str) -> Option<&'static str> {
    tag.strip_prefix(tilo_actions::REQUIRES_HOST_PREFIX)?;
    let feature = match tag {
        tilo_actions::REQUIRES_NATIVE_PATHS_TAG => tilo_files::Feature::NativePaths,
        _ => return Some("This device cannot do this."),
    };
    match tilo_files::availability(feature) {
        tilo_files::Availability::Unsupported(reason) => Some(reason),
        tilo_files::Availability::Available | tilo_files::Availability::Prompt => None,
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
pub(crate) struct Running<K: JobKey = InvocationId> {
    running: HashMap<K, Job>,
}

impl<K: JobKey> Default for Running<K> {
    fn default() -> Self {
        K::running()
    }
}

impl<K: JobKey> Drop for Running<K> {
    fn drop(&mut self) {
        K::stop_all(self);
    }
}

/// What jobs are keyed by: the invocation an effect's job answers, the key
/// a view's job runs under, or the outbox entry a projection's job carries
/// out. What does not depend on the key's type otherwise is compiled once
/// for each of them here, rather than in every app that holds jobs.
pub(crate) trait JobKey: Eq + Hash + Clone + Sized {
    /// No jobs.
    fn running() -> Running<Self>;
    /// Stop every job in `running`.
    fn stop_all(running: &mut Running<Self>);
}

macro_rules! job_key {
    ($($key:ty),*) => {$(
        impl JobKey for $key {
            #[inline(never)]
            fn running() -> Running<Self> {
                Running {
                    running: HashMap::new(),
                }
            }

            #[inline(never)]
            fn stop_all(running: &mut Running<Self>) {
                for (_, job) in running.running.drain() {
                    job.stop();
                }
            }
        }
    )*};
}

job_key!(InvocationId, String, Id);

impl<K: JobKey> Running<K> {
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

    /// Stop the job `key` before it is polled when its caller cancelled it,
    /// or when a signal it answers to went off; `true` when it stopped.
    fn prepare(&mut self, key: &K, cancel: bool) -> bool {
        let stopped = self
            .running
            .get(key)
            .is_some_and(|job| cancel || job.core.fired());
        if stopped {
            self.cancel(key);
        }
        stopped
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
        let (answer, next) = match complete(then, value, invocation) {
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
    pub(crate) fn start(
        &mut self,
        op: &ResourceOperation<'_>,
        host: &mut H,
        doc: Option<&tilo_resource::Doc>,
        handle: impl FnOnce(&mut H, E, &mut EffectCx<R, H>) -> EffectAnswer,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let request = op.request();
        if request.type_id != E::OPERATION {
            return Ok(None);
        }
        let effect = E::from_payload(request.payload).map_err(|error| error.to_string())?;
        let id = effect.id();
        let declaration = E::declaration(id);
        let place = declaration
            .as_ref()
            .and_then(|decl| place_of(decl, &effect));
        let job = admitted(declaration.as_ref(), id, request.kind)?;
        // Another's view of this window shows the row: they are placed at
        // it, and this person's screen does not move.
        if let Some((view, row)) = declaration
            .as_ref()
            .and_then(|decl| shown_elsewhere(decl, op.resource(), self.env.dispatch.me()))
        {
            self.env.live.land(view, row);
            return Ok(Some(ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        let mut cx = EffectCx::invoked(self.env.clone(), invocation_parts(op, doc, place.clone()));
        let mut handling = Some((handle, effect));
        let answer = run_under(Some(request.invocation), &mut || match handling.take() {
            Some((handle, effect)) => handle(host, effect, &mut cx),
            None => Err("an effect's handler ran twice".into()),
        });
        cx.inv.document = None;
        let live = &self.env.live;
        finish_start(
            &mut self.jobs,
            Answered {
                id,
                invocation: request.invocation,
                job,
                answer,
                started: cx.inv.started.take(),
            },
            Tracking {
                started: &mut cx.inv.started,
                wake: &cx.env.wake,
                scope: cx.env.scope.as_ref(),
                cancel_by: cx.inv.cancel_by.as_ref(),
                owner: std::any::TypeId::of::<Then<R, H>>(),
            },
            place.as_ref(),
            &|presence, place| live.land(presence, place),
        )
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
            &mut |then, value, invocation| complete::<R, H>(env, host, then, value, invocation),
        )
    }
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
/// learns as the job's; `land` moves the presence where the effect was
/// told to act once it succeeded.
fn settle_start(
    jobs: &mut Running,
    answered: Answered<'_>,
    reply: &mut dyn FnMut() -> Progress,
    land: &mut dyn FnMut(tilo_actions::PresenceId),
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
    // A presence told where to act is there now.
    if let Some(presence) = ui_core::invocation_scope::current_presence() {
        land(presence);
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

/// What the context of an effect invoked by `op`, told to act at `place`,
/// reading `doc`, holds besides its resource; made outside the generic code
/// so it is compiled once.
#[inline(never)]
fn invocation_parts(
    op: &ResourceOperation<'_>,
    doc: Option<&tilo_resource::Doc>,
    place: Option<tilo_resource::Place>,
) -> Answering {
    let request = op.request();
    let delivered = ui_core::invocation_scope::delivered_invocation()
        .filter(|delivered| *delivered == request.invocation);
    // Keep an immutable snapshot for this effect invocation, including any
    // pending edits. The effect may also read a historical version; neither
    // read borrows the author's document mutably.
    let (document, heads) = lend(doc);
    Answering {
        invocation: Some(request.invocation),
        started: None,
        document,
        heads,
        delivered,
        target: op.target().cloned(),
        place,
        cancel_by: None,
    }
}

/// Start in `jobs` what an effect's handler answered ([`settle_start`]),
/// and move the presence to `place` through `land` once it succeeded.
///
/// # Errors
/// As [`settle_start`].
#[inline(never)]
fn finish_start(
    jobs: &mut Running,
    answered: Answered<'_>,
    tracking: Tracking<'_>,
    place: Option<&tilo_resource::Place>,
    land: &dyn Fn(tilo_actions::PresenceId, tilo_resource::Place),
) -> Result<Option<ResourceOperationStart>, String> {
    let invocation = answered.invocation;
    let Tracking {
        started,
        wake,
        scope,
        cancel_by,
        owner,
    } = tracking;
    let settled = settle_start(
        jobs,
        answered,
        &mut || {
            track_for(
                &mut Tracking {
                    started: &mut *started,
                    wake,
                    scope,
                    cancel_by,
                    owner,
                },
                None,
            )
        },
        &mut |presence| {
            if let Some(place) = place {
                land(presence, place.clone());
            }
        },
    );
    if let Some(ready) = started.take() {
        jobs.insert(invocation, ready);
    }
    settled.map(Some)
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
    // A caller that stopped waiting, and a job left to answer past its
    // caller's wait once that caller gave up, both end here.
    if jobs.prepare(&invocation, cancel) {
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
    /// What the vocabulary declares, independent of any view: what its
    /// surface publishes of it. Only the headless host, which writes the
    /// surface, reads it.
    #[allow(
        dead_code,
        reason = "read where the native headless host writes the surface; a page's build has none"
    )]
    rows: fn() -> Vec<tilo_resource::effect::EffectRow>,
}

/// A view's effect handler, type-erased for the root.
trait ViewHandler {
    fn start(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &tilo_resource::Doc,
        view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Result<Option<ResourceOperationStart>, String>;
    fn poll(
        &mut self,
        invocation: InvocationId,
        cancel: bool,
        view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Option<InvocationUpdate>;
    /// Finish the jobs a window started on its own.
    fn poll_jobs(&mut self, _view: &mut dyn tilo_ui_base::widget::Widget) {}
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
        doc: &tilo_resource::Doc,
        _view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let handle = &mut self.handle;
        // The handler reads the document as it stands
        // ([`EffectCx::document`]), as a hosted surface's does.
        self.runner
            .start(op, &mut (), Some(doc), |(), effect, cx| handle(effect, cx))
    }

    fn poll(
        &mut self,
        invocation: InvocationId,
        cancel: bool,
        _view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Option<InvocationUpdate> {
        self.runner.poll(invocation, cancel, &mut ())
    }
}

/// A registered projection drain ([`ResourceEnv::outbox`](super::ResourceEnv::outbox)),
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
/// Run one entry (boxed) as an invocation: what it answered and the job it
/// started.
type StartEntry = Box<
    dyn FnMut(
        &Id,
        Box<dyn Any>,
        Option<InvocationId>,
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
/// owns its entries ([`ResourceEnv::outbox`](super::ResourceEnv::outbox)). Only
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
    Box<dyn FnOnce(&mut tilo_ui_base::widget::Edits<'_>) -> Result<(), tilo_actions::ActionError>>;

/// Write the settlements in `settling`, then tell the invocations waiting on
/// them how they settled.
fn apply_settlements(settling: &Settling, edits: &mut tilo_ui_base::widget::Edits<'_>) {
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

/// The work each part of a chain owes, by part, with the part's state it
/// was read from: read again only when that part changed.
type OwedParts<P, J> = Rc<RefCell<HashMap<u64, (Rc<<P as Resource>::State>, Vec<(Id, Id, J)>)>>>;

/// What a job of owed work is told by: the item it names and what it asks,
/// so a new ask of the same item is a new job.
fn job_key<J: serde::Serialize>(item: &Id, job: &J) -> Id {
    use std::hash::Hasher as _;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(serde_json::to_string(job).unwrap_or_default().as_bytes());
    Id::from_raw(format!("{}#{:016x}", item.as_str(), hasher.finish()))
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

    /// Run `value` (the entry `id`) as `invocation`. Whether it finished at
    /// once.
    fn start_entry(
        &mut self,
        id: Id,
        value: Box<dyn Any>,
        invocation: Option<InvocationId>,
    ) -> Result<(), String> {
        let (answer, started) = (self.start)(&id, value, invocation)?;
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
            if let Err(error) = self.start_entry(id, value, None) {
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
/// [`ResourceEnv::outbox_awaited`](super::ResourceEnv::outbox_awaited), by those that
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
    pub(crate) fn live(
        dispatch: Dispatcher<R>,
        live: Live<R>,
        shows: Option<ui_core::application_view::WindowShows>,
        opens: &'static [&'static str],
    ) -> Self {
        let env = EffectEnv {
            dispatch,
            live,
            wake: WakeHandle::new(),
            shows,
            opens,
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
    pub(crate) fn invoked(
        dispatch: Dispatcher<R>,
        live: Live<R>,
        shows: Option<ui_core::application_view::WindowShows>,
        opens: &'static [&'static str],
    ) -> Self {
        Self {
            env: Some(EffectEnv {
                dispatch,
                live,
                wake: WakeHandle::new(),
                shows,
                opens,
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
            rows: E::rows,
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
        work: impl FnMut(&Id, T, &mut EffectCx<R>) -> EffectAnswer + 'static,
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
        let start = start_entry(env.clone(), work);
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
            let apply: ApplySettlement =
                Box::new(move |edits| dispatch.apply_now_external(edits, &action));
            Ok(Some((apply, told)))
        });
        let completing = env;
        let complete: Box<Complete<'static>> = Box::new(move |then, value, invocation| {
            complete::<R, ()>(&completing, &mut (), then, value, invocation)
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

    /// Carry out the outside work the parts of `chain` owe, on the device
    /// that owns it ([`ResourceEnv::owed`](super::ResourceEnv::owed)).
    ///
    /// The work is derived from each part as it is ([`owed`]): read again
    /// only for a part that changed, so a chain of thousands of items costs
    /// what its changed part holds. A job is told by the item it names and
    /// what it asks, so asking something else of the same item is new work;
    /// it is carried out once while owed, and is owed no more once the app's
    /// own confirmation (an action on the part) says the outside holds it. A
    /// job that failed for now is tried again once the chain moved on or the
    /// outside changed.
    pub(crate) fn owed<P, J>(
        &self,
        alive: Weak<()>,
        chain: super::Chain<P>,
        owed: impl Fn(&P::State) -> Vec<(Id, J)> + 'static,
        owns: impl Fn(&J) -> bool + 'static,
        work: impl FnMut(&Id, J, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> Projection
    where
        R: 'static,
        P: super::Chained,
        J: serde::Serialize + Clone + 'static,
    {
        let Some(env) = self.working_env().cloned() else {
            return Projection::default();
        };
        let derived: OwedParts<P, J> = Rc::new(RefCell::new(HashMap::new()));
        let pending_of: OwnedEntries = Box::new(move |_state: &dyn Any| {
            let parts = chain.parts();
            let mut derived = derived.borrow_mut();
            derived.retain(|index, _| parts.iter().any(|(held, _)| held == index));
            let mut pending = Vec::new();
            for (index, part) in &parts {
                let fresh = derived
                    .get(index)
                    .is_some_and(|(seen, _)| Rc::ptr_eq(seen, part));
                if !fresh {
                    let jobs = owed(part)
                        .into_iter()
                        .map(|(item, job)| (job_key(&item, &job), item, job))
                        .collect();
                    derived.insert(*index, (Rc::clone(part), jobs));
                }
                if let Some((_, jobs)) = derived.get(index) {
                    pending.extend(jobs.iter().filter(|(_, _, job)| owns(job)).map(
                        |(key, item, job)| {
                            (
                                key.clone(),
                                Box::new((item.clone(), job.clone())) as Box<dyn Any>,
                            )
                        },
                    ));
                }
            }
            Some(pending)
        });
        let mut work = work;
        let start = start_entry(env.clone(), move |_, (item, job): (Id, J), cx| {
            work(&item, job, cx)
        });
        // Nothing to write: the app's confirmation is what settles the work.
        // A failure for good is not tried again here while it stays owed.
        let settle: SettleEntry = Box::new(|_, answer| match answer?.value {
            Some(value) => match serde_json::from_value::<Settled<()>>(value) {
                Ok(Settled::Failed { reason, .. }) => {
                    tracing::warn!(%reason, "owed work failed");
                    Ok(None)
                }
                _ => Ok(None),
            },
            None => Ok(None),
        });
        let completing = env;
        let complete: Box<Complete<'static>> = Box::new(move |then, value, invocation| {
            complete::<R, ()>(&completing, &mut (), then, value, invocation)
        });
        let drain = Rc::new(RefCell::new(OutboxDrain {
            kind: P::KIND,
            owned: pending_of,
            start,
            settle,
            complete,
            abandoned: abandoned::<()>,
            jobs: Running::default(),
            retry: HashMap::new(),
            settled: std::collections::HashSet::new(),
            caused: HashMap::new(),
            revision: 0,
            settling: Rc::clone(&self.inner.borrow().settling),
        }));
        let mut inner = self.inner.borrow_mut();
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

    /// What every published vocabulary declares, whether or not a view of it
    /// is showing: what its surface publishes.
    #[allow(
        dead_code,
        reason = "read where the native headless host writes the surface; a page's build has none"
    )]
    #[inline(never)]
    pub(crate) fn rows(&self) -> Vec<tilo_resource::effect::EffectRow> {
        self.inner.borrow_mut().rows()
    }

    /// Answer `op` with the view handler of its vocabulary.
    pub(crate) fn start(
        &self,
        op: &ResourceOperation<'_>,
        doc: &tilo_resource::Doc,
        view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let operation = op.request().type_id;
        self.with_handlers(|handlers| {
            for entry in handlers {
                if operation == entry.operation
                    && let Some(started) = entry.handler.start(op, doc, view)?
                {
                    return Ok(Some(started));
                }
            }
            Ok(None)
        })
    }

    pub(crate) fn poll(
        &self,
        invocation: InvocationId,
        cancel: bool,
        view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Option<InvocationUpdate> {
        let causal = self
            .inner
            .borrow_mut()
            .causal
            .poll_invocation(invocation, cancel);
        causal.or_else(|| {
            self.with_handlers(|handlers| {
                handlers
                    .iter_mut()
                    .find_map(|entry| entry.handler.poll(invocation, cancel, view))
            })
        })
    }

    /// Finish the jobs the view's windows started
    /// ([`ViewCx::window_jobs`](super::ViewCx::window_jobs)).
    #[inline(never)]
    pub(crate) fn poll_window_jobs(&self, view: &mut dyn tilo_ui_base::widget::Widget) {
        self.with_handlers(|handlers| {
            for entry in handlers {
                entry.handler.poll_jobs(view);
            }
        });
    }

    /// Run `run` over the live handlers, taken out of the registry so a
    /// handler may build view parts that register handlers of their own (a
    /// window mounting a child window as an effect or job finishes).
    fn with_handlers<T>(&self, run: impl FnOnce(&mut Vec<Registered>) -> T) -> T {
        let mut handlers = {
            let mut inner = self.inner.borrow_mut();
            inner.prune();
            std::mem::take(&mut inner.handlers)
        };
        let ran = run(&mut handlers);
        let mut inner = self.inner.borrow_mut();
        handlers.append(&mut inner.handlers);
        inner.handlers = handlers;
        ran
    }

    pub(crate) fn has_causal_drains(&self) -> bool {
        self.inner.borrow().causal.registered()
    }

    pub(crate) fn start_document_effects(
        &self,
        operation: &ResourceOperation<'_>,
        applied: &tilo_ui_base::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.inner
            .borrow_mut()
            .causal
            .start(operation, applied, read_state::<R>)
    }

    /// Write the outbox settlements learned since the last settle pass, then
    /// tell the invocations waiting on them how they settled.
    #[inline(never)]
    pub(crate) fn apply_settlements(&self, edits: &mut tilo_ui_base::widget::Edits<'_>) {
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

    #[allow(
        dead_code,
        reason = "read where the native headless host writes the surface; a page's build has none"
    )]
    fn rows(&mut self) -> Vec<tilo_resource::effect::EffectRow> {
        self.prune();
        self.published
            .iter()
            .flat_map(|entry| (entry.rows)())
            .collect()
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
