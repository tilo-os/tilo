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
//! [`ViewCx::outbox`](super::ViewCx::outbox): the framework runs the app's
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
use tilo_resource::{ActionRef, Effect, Id, Invoke, Outbox, Path, Resource, Settled};
use ui_core::WakeHandle;
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart};
use ui_core::invocation_scope::{ResourceEffectScope, ResourceInvocationScope};
use ui_core::portal::{ContentsOutcome, PickRequest, PickedContents, PortalOutcome, SaveOutcome};

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
    /// ([`ViewCx::outbox`](super::ViewCx::outbox)). [`Self::value`] with the
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

/// A job a handler or a completion started, not yet tracked.
struct Started<R: Resource, H> {
    events: Receiver<JobEvent>,
    then: Option<Then<R, H>>,
    cancelled: Arc<AtomicBool>,
    task: Option<ui_core::rt::JoinHandle<()>>,
    source: Option<Source>,
    custody: Option<ResourceInvocationScope>,
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
    /// The resource's document while a surface's handler answers an
    /// invocation, lent to it for reading.
    document: Option<Rc<RefCell<tilo_resource::Doc>>>,
    /// The lent document's heads, taken when it was lent.
    heads: Vec<ui_core::automerge::ChangeHash>,
    custody: Option<ResourceInvocationScope>,
    worker_custody: Option<ResourceEffectScope>,
    target: Option<ui_core::host_operation::OperationTarget>,
}

/// The resource as it was at a past version a view asked for: its
/// document there, the heads that name it and the name a caller gives it
/// back ([`EffectCx::version`]).
pub struct DocumentVersion {
    pub doc: tilo_resource::Doc,
    pub heads: Vec<ui_core::automerge::ChangeHash>,
    pub name: String,
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
        }
    }

    fn with_custody(mut self, custody: Option<ResourceInvocationScope>) -> Result<Self, String> {
        self.worker_custody = custody
            .as_ref()
            .map(ResourceInvocationScope::effect_scope)
            .transpose()?;
        if let Some(scope) = &custody {
            self.env.dispatch = self.env.dispatch.for_custody(scope.clone());
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

    /// The actual source invocation this work continues. Ordinary background
    /// projection drains have none; causal drains retain the current Document
    /// caller. Its source identity may differ from the local reply operation.
    pub fn invocation(&self) -> Option<InvocationId> {
        self.invocation
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
            custody: self.worker_custody.clone(),
        };
        if let Some(previous) = self.started.replace(Started {
            events: receiver,
            then,
            cancelled: Arc::clone(&cancelled),
            task: None,
            source: None,
            custody: self.custody.clone(),
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
            let mut future = std::pin::pin!(future);
            let value = std::future::poll_fn(|cx| {
                match progress.with_effect(|| Ok(future.as_mut().poll(cx))) {
                    Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
                    Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
                    Err(error) => std::task::Poll::Ready(Err(error)),
                }
            })
            .await;
            match value {
                Ok(value) => progress.finish(Box::new(value)),
                Err(error) => progress.fail(error),
            }
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
            match progress.with_effect(|| Ok(work(progress.clone()))) {
                Ok(value) => progress.finish(Box::new(value)),
                Err(error) => progress.fail(error),
            }
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
        let (progress, _) = self.track(Some(Self::then(then)));
        EffectReply {
            progress,
            completion: true,
        }
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

    /// Ask the host to show what a launch naming `arguments` opens in this
    /// window, in place of the record it shows ([`OpenHere::open`]), then
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
        let (progress, _) = self.track(Some(Self::then(then)));
        if let Some(started) = self.started.as_mut() {
            started.source = Some(Box::new(move || {
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
    custody: Option<ResourceInvocationScope>,
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
            custody: started.custody,
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

impl<R: Resource, H, K> Drop for Running<R, H, K> {
    fn drop(&mut self) {
        for (_, job) in self.running.drain() {
            job.stop();
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
        if let Some(error) = self
            .running
            .get(key)
            .and_then(|job| job.custody.as_ref())
            .and_then(|custody| custody.check().err())
        {
            self.cancel(key);
            return Some(Polled::Finished(Some(Err(failure(error)))));
        }
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
        if let Some(error) = job
            .custody
            .as_ref()
            .and_then(|custody| custody.check().err())
        {
            self.cancel(key);
            return Some(Polled::Finished(Some(Err(failure(error)))));
        }
        let value = match event {
            JobEvent::Failed(error) => {
                self.cancel(key);
                return Some(Polled::Finished(Some(Err(error))));
            }
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
        let mut cx = match EffectCx::new(env.clone(), invocation).with_custody(job.custody.clone())
        {
            Ok(cx) => cx,
            Err(error) => return Some(Polled::Finished(Some(Err(failure(error))))),
        };
        let answer = {
            let _acting = ActingFor::enter(invocation);
            match &job.custody {
                Some(custody) => custody
                    .with_effect(|| then(host, value, &mut cx))
                    .and_then(|answer| custody.check().map(|()| answer)),
                None => then(host, value, &mut cx),
            }
        };
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
        let declaration = E::declarations().into_iter().find(|decl| decl.id == id);
        // A view only changes what its caller sees and is admitted as one;
        // an effect invoked as a view would skip the checks an effect gets.
        if declaration.as_ref().map(|decl| decl.kind) != Some(request.kind) {
            return Err(format!("`{id}` was invoked as another kind of operation"));
        }
        let job = declaration.is_some_and(|decl| decl.is_job());
        let mut cx = EffectCx::new(self.env.clone(), Some(request.invocation))
            .with_custody(custody.cloned())?;
        cx.target = op.target().cloned();
        // Keep an immutable snapshot for this effect invocation, including any
        // pending edits. The effect may also read a historical version; neither
        // read borrows the author's document mutably.
        cx.document = doc.map(|doc| {
            cx.heads = doc.base_heads();
            Rc::new(RefCell::new(doc.clone()))
        });
        let answer = {
            let _acting = ActingFor::enter(Some(request.invocation));
            match custody {
                Some(custody) => custody
                    .with_effect(|| handle(host, effect, &mut cx))
                    .and_then(|answer| custody.check().map(|()| answer)),
                None => handle(host, effect, &mut cx),
            }
        };
        cx.document = None;
        let started = cx.started.take();
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => {
                if let Some(started) = started {
                    started.stop();
                }
                return Err(error);
            }
        };
        match (answer, started, job) {
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
            .start(op, &mut (), None, |(), effect, cx| handle(effect, cx))
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

type Owns<R, T> = Box<dyn Fn(&<R as Resource>::State, &T) -> bool>;
type RunEntry<R, T> = Box<dyn FnMut(&Id, T, &mut EffectCx<R>) -> EffectAnswer>;

/// The drain of one [`Outbox`](tilo_resource::Outbox) on the device that
/// owns its entries ([`ViewCx::outbox`](super::ViewCx::outbox)).
struct OutboxDrain<R: Resource, T, O> {
    env: EffectEnv<R>,
    outbox: Path<R, Outbox<T, O>>,
    owns: Owns<R, T>,
    run: RunEntry<R, T>,
    jobs: Running<R, (), Id>,
    /// Entries whose run failed for now, and the revision it failed at: run
    /// again once the document moved on or the outside changed.
    retry: HashMap<Id, u64>,
    /// Entries this drain settled, never run again here while their
    /// settlement is on its way into the state.
    settled: std::collections::HashSet<Id>,
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

impl<R, T, O> OutboxDrain<R, T, O>
where
    R: Resource<State = R> + tilo_resource::Model + 'static,
    T: tilo_resource::Model + Clone + 'static,
    O: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
{
    /// Record how the entry `id` settled, or leave it pending to run again.
    fn finish(&mut self, id: &Id, answer: Result<ActionOutput, String>, revision: u64) -> bool {
        match answer.and_then(|output| settlement::<O>(&output)) {
            Ok(settled) => {
                self.settled.insert(id.clone());
                self.env.dispatch.submit(self.outbox.settle(id, &settled));
                true
            }
            Err(error) => {
                tracing::warn!(%error, kind = R::KIND, "an outbox entry stays pending");
                self.retry.insert(id.clone(), revision);
                false
            }
        }
    }
}

impl<R, T, O> Drainer<R> for OutboxDrain<R, T, O>
where
    R: Resource<State = R> + tilo_resource::Model + 'static,
    T: tilo_resource::Model + Clone + 'static,
    O: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
{
    fn run(&mut self, state: &R::State, revision: u64) {
        let Some(outbox) = self.outbox.get(state) else {
            return;
        };
        let pending: Vec<(Id, T)> = outbox
            .pending()
            .filter(|(_, value)| (self.owns)(state, value))
            .map(|(id, value)| (id.clone(), value.clone()))
            .collect();
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
            let mut cx = EffectCx::new(self.env.clone(), None);
            let answer = (self.run)(&id, value, &mut cx);
            match (answer, cx.started.take()) {
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
                    self.finish(&id, answer, revision);
                }
            }
        }
    }

    fn poll(&mut self, revision: u64) -> bool {
        let mut finished = false;
        for id in self.jobs.keys() {
            while let Some(polled) = self.jobs.poll(&id, &mut (), &self.env, None) {
                let Polled::Finished(result) = polled else {
                    continue;
                };
                if let Some(result) = result {
                    self.finish(&id, result.map_err(|error| error.message), revision);
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
    causal: causal::CausalEffects<R>,
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
            causal: causal::CausalEffects::default(),
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

    pub(crate) fn outbox<T, O>(
        &self,
        alive: Weak<()>,
        outbox: Path<R, Outbox<T, O>>,
        owns: impl Fn(&R::State, &T) -> bool + 'static,
        run: impl FnMut(&Id, T, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> Projection
    where
        R: Resource<State = R> + tilo_resource::Model + 'static,
        T: tilo_resource::Model + Clone + 'static,
        O: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
    {
        let Some(env) = self.env.clone() else {
            return Projection::default();
        };
        let mut inner = self.inner.borrow_mut();
        inner.drains.push(Drains {
            alive,
            drain: Box::new(OutboxDrain {
                env,
                outbox,
                owns: Box::new(owns),
                run: Box::new(run),
                jobs: Running::default(),
                retry: HashMap::new(),
                settled: std::collections::HashSet::new(),
            }),
        });
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
            if op.with_custody().0.type_id == entry.operation
                && let Some(started) = entry.handler.start(op)?
            {
                return Ok(Some(started));
            }
        }
        Ok(None)
    }

    pub(crate) fn poll(&self, invocation: InvocationId, cancel: bool) -> Option<InvocationUpdate> {
        let mut inner = self.inner.borrow_mut();
        if let Some(update) = inner.causal.poll_invocation(invocation, cancel) {
            return Some(update);
        }
        inner
            .handlers
            .iter_mut()
            .find_map(|entry| entry.handler.poll(invocation, cancel))
    }

    pub(crate) fn has_causal_drains(&self) -> bool {
        self.inner.borrow().causal.registered()
    }

    pub(crate) fn start_document_effects(
        &self,
        operation: &ResourceOperation<'_>,
        applied: &ui_core::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.inner.borrow_mut().causal.start(operation, applied)
    }

    pub(crate) fn drain_causal<K: Eq + Hash + Clone + 'static, T: 'static>(
        &self,
        alive: Weak<()>,
        pending: impl Fn(&R::State) -> Vec<(K, T)> + 'static,
        run: impl FnMut(T, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) {
        if let Some(env) = self.env.clone() {
            self.inner
                .borrow_mut()
                .causal
                .register(alive, env, pending, run);
        }
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
        runner_on(Dispatcher::new("test", Probe::KIND, false, None))
    }

    fn runner_on(dispatch: Dispatcher<Probe>) -> EffectRunner<Probe, (), Work> {
        let mut doc = tilo_resource::Doc::new();
        Probe::initialize(&mut doc).unwrap();
        EffectRunner::new(EffectEnv {
            dispatch,
            live: Live::new(Reader::<Probe>::read(&doc).unwrap()),
            wake: WakeHandle::detached(),
        })
    }

    #[test]
    fn what_an_effect_submits_continues_its_invocation() {
        let dispatch = Dispatcher::new("test", Probe::KIND, false, None);
        let mut runner = runner_on(dispatch.clone());
        let invocation = InvocationId::new();
        let payload = payload(&Work::Count);
        let op = ResourceOperation::local(
            invocation,
            tilo_actions::OperationKind::Effect,
            Work::OPERATION,
            &payload,
        );
        runner
            .start(&op, &mut (), None, |(), _, cx| {
                cx.dispatcher().effect(&Work::Ping);
                Ok(cx.thread(
                    |_| (),
                    |(), (), cx| {
                        cx.dispatcher().effect(&Work::Ping);
                        Ok(Effected::done())
                    },
                ))
            })
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !runner
            .poll(invocation, false, &mut ())
            .is_some_and(|update| update.is_terminal())
        {
            assert!(std::time::Instant::now() < deadline, "the job finishes");
            std::thread::yield_now();
        }
        dispatch.effect(&Work::Ping);
        let continues = std::iter::from_fn(|| dispatch.take_attributed())
            .map(|(_, on_behalf_of)| on_behalf_of)
            .collect::<Vec<_>>();
        assert_eq!(
            continues,
            [Some(invocation), Some(invocation), None],
            "the handler's and the completion's continue the invocation; \
             the app's own continues nothing"
        );
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
            .start(&op, &mut (), None, |(), _, cx| {
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
    fn a_failed_handler_cancels_the_job_it_already_started() {
        let mut runner = runner();
        let invocation = InvocationId::new();
        let payload = payload(&Work::Count);
        let op = ResourceOperation::local(
            invocation,
            tilo_actions::OperationKind::Effect,
            Work::OPERATION,
            &payload,
        );
        let mut progress = None;
        let result = runner.start(&op, &mut (), None, |(), _, cx| {
            let reply = cx.reply();
            progress = Some(reply.progress().clone());
            Err("handler refused after starting work".into())
        });
        assert!(result.is_err());
        assert!(progress.unwrap().is_cancelled());
        assert!(runner.poll(invocation, false, &mut ()).is_none());
    }

    #[test]
    fn dropping_the_handler_cancels_its_retained_jobs() {
        let mut runner = runner();
        let payload = payload(&Work::Count);
        let op = ResourceOperation::local(
            InvocationId::new(),
            tilo_actions::OperationKind::Effect,
            Work::OPERATION,
            &payload,
        );
        let mut progress = None;
        assert!(matches!(
            runner
                .start(&op, &mut (), None, |(), _, cx| {
                    let reply = cx.reply();
                    progress = Some(reply.progress().clone());
                    Ok(Effected::Pending)
                })
                .unwrap(),
            Some(ResourceOperationStart::Accepted)
        ));
        let progress = progress.unwrap();
        assert!(!progress.is_cancelled());
        drop(runner);
        assert!(progress.is_cancelled());
        let mut work_ran = false;
        assert!(
            progress
                .with_effect(|| {
                    work_ran = true;
                    Ok(())
                })
                .is_err()
        );
        assert!(!work_ran, "retired work must not enter its effect callback");
    }

    /// A handler reads the document it answers about, now and at a past
    /// version, and the document is back with its owner once it answered.
    #[test]
    fn a_handler_reads_the_document_now_and_at_a_version() {
        use ui_core::automerge::{ReadDoc, transaction::Transactable};
        let mut runner = runner();
        let mut doc = tilo_resource::Doc::new();
        doc.put(ui_core::automerge::ROOT, "count", 1_i64).unwrap();
        let first = tilo_resource::history::commit_stamped(&mut doc).unwrap();
        doc.put(ui_core::automerge::ROOT, "count", 2_i64).unwrap();
        tilo_resource::history::commit_stamped(&mut doc);
        let payload = payload(&Work::Ping);
        let op = ResourceOperation::local(
            InvocationId::new(),
            tilo_actions::OperationKind::Effect,
            Work::OPERATION,
            &payload,
        );
        let count = |doc: &tilo_resource::Doc| {
            doc.get(ui_core::automerge::ROOT, "count")
                .unwrap()
                .and_then(|(value, _)| value.as_i64())
        };
        let before = (doc.base_heads(), doc.pending_ops());
        let answered = runner
            .start(&op, &mut (), Some(&doc), |(), _, cx| {
                let now = count(&cx.document().expect("lent to the handler"));
                let past = cx.version(&first.to_string())?;
                assert_eq!(past.heads, vec![first]);
                Ok(Effected::value([now, count(&past.doc)]))
            })
            .unwrap();
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
        assert!(matches!(
            answered,
            Some(ResourceOperationStart::Completed(output))
                if output.value == Some(serde_json::json!([2, 1]))
        ));
        assert_eq!(count(&doc), Some(2), "the document is back with its owner");
        let refused = runner
            .start(&op, &mut (), None, |(), _, cx| {
                cx.version(&first.to_string()).map(|_| Effected::done())
            })
            .unwrap_err();
        assert!(refused.contains("no document"), "{refused}");
    }

    /// An effect waiting to show another record here: the opener it asked,
    /// and the requests it made.
    fn opener() -> (
        super::super::OpenHere,
        Rc<RefCell<Vec<ui_core::application::ReplacementOutcome>>>,
    ) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let requests = Rc::clone(&asked);
        let opener = super::super::OpenHere::new(move |_| {
            let outcome = ui_core::application::ReplacementOutcome::default();
            requests.borrow_mut().push(outcome.clone());
            outcome
        });
        (opener, asked)
    }

    fn open_here_job(
        runner: &mut EffectRunner<Probe, (), Work>,
        opener: &super::super::OpenHere,
    ) -> InvocationId {
        let invocation = InvocationId::new();
        let payload = payload(&Work::Count);
        let op = ResourceOperation::local(
            invocation,
            tilo_actions::OperationKind::Effect,
            Work::OPERATION,
            &payload,
        );
        let opener = opener.clone();
        runner
            .start(&op, &mut (), None, move |(), _, cx| {
                Ok(cx.open_here(&opener, &["b.pdf".into()], |(), answer, _| {
                    answer.map(|()| Effected::value("shown"))
                }))
            })
            .unwrap();
        invocation
    }

    #[test]
    fn opening_here_answers_with_the_hosts_answer_or_withdraws_when_cancelled() {
        let (opener, asked) = opener();
        let mut runner = runner();

        let shown = open_here_job(&mut runner, &opener);
        assert!(
            runner.poll(shown, false, &mut ()).is_none(),
            "waits for the host"
        );
        asked.borrow()[0].answer(Ok(()));
        assert!(matches!(
            runner.poll(shown, false, &mut ()),
            Some(InvocationUpdate::Succeeded { output, .. })
                if output.value == Some(serde_json::json!("shown"))
        ));

        let refused = open_here_job(&mut runner, &opener);
        asked.borrow()[1].answer(Err("the file is gone".into()));
        assert!(matches!(
            runner.poll(refused, false, &mut ()),
            Some(InvocationUpdate::Failed { error, .. }) if error.message == "the file is gone"
        ));

        let cancelled = open_here_job(&mut runner, &opener);
        assert!(matches!(
            runner.poll(cancelled, true, &mut ()),
            Some(InvocationUpdate::Cancelled { .. })
        ));
        assert!(
            !asked.borrow()[2].withdraw(),
            "the cancelled effect withdrew its request before the host took it"
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
            .start(&op, &mut (), None, |(), _, cx| {
                let _reply = cx.reply();
                Ok(Effected::Pending)
            })
            .unwrap_err();
        assert!(refused.contains("#[effect(job)]"), "{refused}");
        let answered = runner
            .start(&op, &mut (), None, |(), _, _| Ok(Effected::value("pong")))
            .unwrap();
        assert!(matches!(
            answered,
            Some(ResourceOperationStart::Completed(output))
                if output.value == Some(serde_json::json!("pong"))
        ));
    }
}
