//! The contract a custom widget or renderer implements to take part in a
//! resource: collaboration, history, undo, permissions and the action door.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::{
    Arc, Weak as SyncWeak,
    atomic::{AtomicBool, Ordering},
};
use ui_core::widget::{AppModel, ModelPath};

use tilo_resource::{ActionDecl, Effect, NoEffect, Resource, SearchEntry};
use ui_core::automerge::transaction::Transactable as _;
use ui_core::automerge::{AutoCommit, ObjId};
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart, ResourceOperations};
use ui_core::presence::PresenceKey;
use ui_core::widget::{Decorator, Widget};
use ui_core::{PresencePayload, WakeHandle};

use super::controls::Actions;
use super::dispatch::Dispatcher;
use super::effect::{EffectAnswer, EffectCx, EffectRunner, EffectStatus, Effects, Jobs, OwnedJobs};
use super::live::Live;
use super::rows::{LiveRow, outside_document, resolved_outside_document};

/// A hosted surface's publication and job lifetime. Retained contexts contain
/// this revocable scope, never a strong reference to its publication token.
pub(crate) struct SurfaceScope {
    token: RefCell<Option<Rc<()>>>,
    parent: Weak<()>,
    children: RefCell<Vec<Weak<Self>>>,
    jobs: RefCell<Vec<SyncWeak<AtomicBool>>>,
}
/// Keep `now` in `statuses` when it says something else. Not generic, so
/// every app's hosts share it.
#[inline(never)]
fn note_statuses(statuses: &RefCell<Vec<EffectStatus>>, now: Vec<EffectStatus>) {
    if *statuses.borrow() != now {
        *statuses.borrow_mut() = now;
    }
}

/// Have a hosted surface of `scope` stage what `stages` run, on its next
/// sync through `pending`, while the scope is active. Not generic, so every
/// app's hosts share it.
#[inline(never)]
fn stage_hosted(stages: &super::stage::Stages, scope: Rc<SurfaceScope>, pending: Staging) {
    stages.push(
        scope.alive(),
        Box::new(move |scenario| {
            if scope.active() {
                *pending.borrow_mut() = Some(scenario.map(str::to_owned));
            }
        }),
    );
}

impl SurfaceScope {
    /// A scope alive while `parent` is, a child of the surface scope
    /// `within` when there is one. Not generic, so every app's hosts share
    /// it.
    #[inline(never)]
    fn under(parent: Weak<()>, within: Option<&Rc<Self>>) -> Rc<Self> {
        let scope = Self::new(parent);
        if let Some(within) = within {
            let mut children = within.children.borrow_mut();
            children.retain(|child| child.strong_count() > 0);
            children.push(Rc::downgrade(&scope));
        }
        scope
    }

    fn new(parent: Weak<()>) -> Rc<Self> {
        Rc::new(Self {
            token: RefCell::new(Some(Rc::new(()))),
            parent,
            children: RefCell::default(),
            jobs: RefCell::default(),
        })
    }
    fn alive(&self) -> Weak<()> {
        self.token
            .borrow()
            .as_ref()
            .map_or_else(Weak::new, Rc::downgrade)
    }
    pub(crate) fn active(&self) -> bool {
        if self.parent.strong_count() == 0 {
            self.retire();
        }
        self.token.borrow().is_some()
    }
    pub(crate) fn track(&self, cancelled: &Arc<AtomicBool>) {
        if self.active() {
            let mut jobs = self.jobs.borrow_mut();
            jobs.retain(|job| job.strong_count() > 0);
            jobs.push(Arc::downgrade(cancelled));
        } else {
            cancelled.store(true, Ordering::Release);
        }
    }
    fn retire(&self) {
        self.token.borrow_mut().take();
        for child in self
            .children
            .borrow_mut()
            .drain(..)
            .filter_map(|child| child.upgrade())
        {
            child.retire();
        }
        for job in self
            .jobs
            .borrow_mut()
            .drain(..)
            .filter_map(|job| job.upgrade())
        {
            job.store(true, Ordering::Release);
        }
    }
}

/// A custom widget or renderer — a timeline, a canvas, a GPU viewport, a page
/// layout engine — taking part in a resource.
///
/// Pixels, layout and input stay the toolkit's `Widget` contract, unchanged.
/// This trait adds only what the framework cannot infer:
///
/// * **Read** — [`present`](Self::present) receives the resource state
///   whenever it changes: a local edit, a collaborator's, an undo. In History
///   mode the framework builds the surface again from a read-only `ViewCx`
///   and presents a *past* version, so time travel needs no extra code as
///   long as the surface draws from what it was presented. A surface over a
///   large resource reads through [`SurfaceCx::live`] instead of copying,
///   and demands the parts it shows ([`Live::demand`]) from the document its
///   `sync` receives — the live one, or the past version in History mode.
/// * **Write** — edits go through the [`Dispatcher`] handed to
///   [`attach`](Self::attach): a named action, or any typed action value via
///   [`Dispatcher::apply`]. The host checks permissions, applies, records one
///   undo step per action, attributes it in history and replicates it. A
///   surface never writes the document.
/// * **Undo granularity** — one action is one undo step. Preview a gesture
///   locally (a drag, a scrub) and dispatch one action when it ends; a
///   gesture that is several edits goes in one [`Dispatcher::group`] (or
///   holds a [`Dispatcher::begin_group`] guard for the input event), so one
///   Ctrl+Z takes all of it back.
/// * **Permissions** — [`RecordEnv::read_only`](super::RecordEnv::read_only) says the participant may
///   only view; present accordingly. It follows role changes while the app
///   runs, and [`read_only_changed`](Self::read_only_changed) announces each
///   one. Refused actions change nothing.
/// * **Confirmation** — a typed value whose
///   [`risk`](tilo_actions::Action::risk) is destructive (deleting a clip)
///   waits for the user's confirmation, like a declared destructive action.
///   When an edit is refused or held for confirmation, the surface is
///   presented the stored state again, undoing any optimistic preview; a
///   confirmed edit then arrives as an ordinary change.
/// * **Presence** — report where the user is as a [`ResourceSelection`] from
///   the toolkit's `Widget::presence_payload` (one line:
///   `selection.presence::<R>(app_id)`), and draw collaborators from the
///   `PresenceCtx` every draw receives, under the same keys.
/// * **Effects** — work with no document delta (back up now, connect, run a
///   query) is the surface's effect vocabulary `E`, a `#[derive(Effect)]`
///   enum: implement `ResourceSurface<R, E>` and answer each effect in
///   [`effect`](Self::effect) with `&mut self`, starting a job through the
///   [`EffectCx`] when it takes time. Every caller — a button
///   ([`Dispatcher::effect`]), the palette, an agent — reaches it the same
///   way; [`effects`](Self::effects) says which are unavailable right now.
/// * **Run mode** — start work that reaches outside the process (a decoder
///   service, a socket, a probe) only when [`RecordEnv::mode`](super::RecordEnv::mode) is live. A
///   capture reads [`RecordEnv::fixture`](super::RecordEnv::fixture) instead; a past version starts
///   nothing. Work still in flight that the frame shows (a page render, a
///   decode) is reported by the toolkit's `Widget::has_pending_visual_work`;
///   a capture waits for it.
/// * **One published tree** — what the surface offers the palette, agents
///   and scripts joins the resource's own publication:
///   [`bound_declarations`](Self::bound_declarations) for edits bound to
///   where the user is (delete the selected clip, bookmark the current
///   folder), and [`live_rows`](Self::live_rows) for the live external
///   entities it shows (processes, containers), which carry only view and
///   effect operations. A document action the surface's own
///   `PublishesResources` offers is dropped, so nothing reaches the
///   document beside the resource's actions.
pub trait ResourceSurface<R: Resource, E: Effect = NoEffect>: Widget {
    /// The resource state to show.
    ///
    /// Keeping `state` past this call is allowed but not free: while it is
    /// held, the next change brings a copy up to date. A copy shares what
    /// the change did not touch — every [`Collection`](tilo_resource::Collection)
    /// item, untouched `Keyed`/`Ordered` fields, the untouched chunks of a
    /// `Map`, `Set` or `Grid` — but plain fields (a `Vec`, a `BTreeMap`) are
    /// copied whole on every change, so a large one belongs in one of those,
    /// and a surface keeps the parts it needs rather than the `Rc` when the
    /// state has large plain ones.
    fn present(&mut self, state: &Rc<R::State>);

    fn supports_device_shell(&self) -> bool {
        false
    }

    /// Answer a private owner operation from the authenticated hosting shell.
    ///
    /// # Errors
    /// Refuses unsupported or unavailable device-shell operations.
    fn device_shell_operation(
        &mut self,
        _operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        Err("This surface has no device shell".into())
    }

    /// A trusted hosting shell owner selected the device-shell target. No grant is implied.
    fn request_device_shell(&mut self) -> bool {
        false
    }

    /// The latest bounded query from the platform's canonical search source.
    /// Runs on the UI thread before sync, coalescing queries between frames.
    /// A surface may refresh remote results through its attached jobs; expose
    /// their entries through `search_entries`, rather than another provider.
    /// Never called for a History view and lends no document.
    fn search_query(&mut self, query: &ui_core::search::SearchQuery) {
        let _ = query;
    }

    /// Whether results for the latest [`search_query`](Self::search_query)
    /// are still coming (a server is still looking). A search already
    /// showing waits for them and shows them, with what `search_entries`
    /// gives once this turns false, without anyone typing again. Read after
    /// every sync.
    fn search_pending(&self) -> bool {
        false
    }

    /// Consume a request to retry this window's failed record save, once.
    /// The framework's record owner performs the retry after releasing its
    /// document loan; the surface never writes or flushes storage itself.
    /// History surfaces cannot request a retry.
    fn take_save_retry(&mut self) -> bool {
        false
    }

    /// A pending device-local request for the hosting shell, consumed once.
    /// The host answers it through its permission flow; this callback lends no document.
    fn take_host_intent(&mut self) -> Option<ui_core::widget::AppHostIntent> {
        None
    }

    /// The channel edits go through, and whether they will be accepted.
    /// Called once, before the first [`present`](Self::present).
    fn attach(&mut self, cx: SurfaceCx<R>) {
        let _ = cx;
    }

    /// A participant's action — an agent's verb, a collaborator's edit
    /// taken through the door — left them at `place`, as the resource says
    /// it lands ([`Resource::lands`](tilo_resource::Resource::lands)).
    /// Called after [`present`](Self::present) showed the state holding it.
    /// Presence draws them there already; a surface that acts at a
    /// participant's place (typing at an agent's caret) reads it here, or
    /// any time through [`Live::place_of`](super::Live::place_of), instead
    /// of keeping its own copy.
    fn landed(&mut self, participant: &tilo_actions::ParticipantId, place: &tilo_resource::Place) {
        let _ = (participant, place);
    }

    /// Whether the surface is told of the actions applied here
    /// ([`applied`](Self::applied)). A surface that implements it says so
    /// here; no other surface's window keeps them.
    const APPLIED: bool = false;

    /// `action` was applied to the resource here through the door: invoked
    /// from the palette, a script or an agent, or submitted by the surface's
    /// own control, and only once the person agreed when it asks first.
    /// Called after [`present`](Self::present) showed the state holding it;
    /// never for a collaborator's change merged in, nor for an undo or a
    /// restore.
    ///
    /// What an action means on this device beyond the document (a file
    /// grant a removal forgets) is carried out here, so it happens when the
    /// action applies and never when the person cancelled it. Asked only of
    /// a surface that sets [`APPLIED`](Self::APPLIED).
    fn applied(&mut self, action: &R::Action) {
        let _ = action;
    }

    /// The participant's permission to edit changed while the app runs: a
    /// role granted or taken away. Present accordingly. The dispatcher already
    /// refuses or accepts edits under the new permissions.
    fn read_only_changed(&mut self, read_only: bool) {
        let _ = read_only;
    }

    /// Stage view state for a screenshot scenario (`None` for a plain
    /// `--shots`): a selection, an open menu, a drag in flight. Called at the
    /// start of the sync after the scenario's actions applied and were laid
    /// out, so the surface has been presented their result, knows its
    /// geometry, and its own `sync` follows with the document. Edits it dispatches here are submitted like any other and
    /// applied before the frame is captured. Must be deterministic: the same
    /// scenario stages the same scene on every run. Never called in a live
    /// window.
    fn stage(&mut self, scenario: Option<&str>) {
        let _ = scenario;
    }

    /// Answer `effect`, invoked by a control, the palette, an agent or a
    /// script: finish now ([`Effected::done`](super::Effected::done)), or
    /// start a job with [`EffectCx::thread`], [`EffectCx::spawn`] or
    /// [`EffectCx::reply`] and answer [`Effected::Pending`](super::Effected::Pending)
    /// (only for an effect declared `job`). A job's completion runs on the
    /// UI thread with `&mut self` again, and changes the document through
    /// the context's dispatcher. Never called in History mode.
    ///
    /// # Errors
    /// Returns why the effect failed, which its caller is told.
    fn effect(&mut self, effect: E, cx: &mut EffectCx<R, Self>) -> EffectAnswer
    where
        Self: Sized,
    {
        let _ = cx;
        Err(format!("`{}` has no handler", effect.id()))
    }

    /// The effects that differ from their declaration right now, as the
    /// surface's state says: unavailable while the engine is offline,
    /// relabelled while recording. Read after every sync, so the published
    /// actions follow the surface.
    fn effects(&self) -> Vec<EffectStatus> {
        Vec::new()
    }

    /// The resource's own actions that differ from their declaration right
    /// now, as the surface's live state says (`Camera::SET_MODE` unavailable
    /// while recording, with why). Read whenever the resource is published,
    /// so the palette, agents and the door follow the surface: an action
    /// unavailable here is refused with its reason.
    fn availability(&self) -> Vec<EffectStatus> {
        Vec::new()
    }

    /// Edits bound to the surface's view state — the selection, the caret,
    /// the playhead, the folder or page shown — as typed builders of the
    /// resource's action, published on the resource beside its declared
    /// actions. Invoking one dispatches the action it builds through the
    /// same permission check, confirmation and undo as any other. An id the
    /// resource already publishes keeps the resource's action, unless the
    /// bound one refines it ([`tilo_resource::bind_declaration`]): same
    /// parameters, permission and risk, adding only where it was invoked
    /// from, such as the caret an append leaves its writer at.
    fn bound_declarations(&self) -> Vec<ActionDecl<R>> {
        Vec::new()
    }

    /// The live external entities the surface shows, published as children
    /// of the resource and merged by id with its document children. Keep it
    /// to what fits a published tree (the rows on screen, a bounded page)
    /// and resolve the rest in [`live_row`](Self::live_row).
    fn live_rows(&self) -> Vec<LiveRow> {
        Vec::new()
    }

    /// The live row `id`, when [`live_rows`](Self::live_rows) leaves it out,
    /// so an action addressed to any entity the surface knows still reaches
    /// it.
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        let _ = id;
        None
    }

    /// Whether [`row_declarations`](Self::row_declarations) binds edits on
    /// any row. A surface that implements it says so here; every other
    /// surface's rows are published without asking it about each one, and
    /// a row it hands out again is published as it was.
    const ROW_DECLARATIONS: bool = false;

    /// Edits of the resource bound to the live row `row` (rename *this*
    /// file): typed builders of the resource's action, published on that row
    /// and applied through the same permission check, confirmation and undo
    /// as the resource's own declarations. A row itself carries only view
    /// and effect operations; this is how it offers a document change.
    /// Asked only of a surface that sets
    /// [`ROW_DECLARATIONS`](Self::ROW_DECLARATIONS).
    fn row_declarations(&self, row: &str) -> Vec<ActionDecl<R>> {
        let _ = row;
        Vec::new()
    }

    /// Search entries for what the surface shows that the resource does not
    /// hold — a page's text, a book's outline, the processes running —
    /// indexed with the resource's own ([`Resource::search`]) as the app's
    /// one search source. Read whenever someone searches, so keep it to what
    /// the surface already has at hand.
    fn search_entries(&self) -> Vec<SearchEntry> {
        Vec::new()
    }

    /// Whether the secrets behind this surface's secret-bearing search
    /// entries ([`SearchEntry::secret_bearing`]) are unlocked now: a vault
    /// that opened and found its key. The system's search index finds those
    /// entries only while the app last said `Some(true)`, and keeps them
    /// after the app closes. `None` says nothing: a surface without secrets,
    /// or one that opens none (a past version).
    fn secrets_unlocked(&self) -> Option<bool> {
        None
    }

    /// Whether the system's search index keeps this surface's
    /// [`search_entries`](Self::search_entries) after the app closes, so
    /// they are found whether or not it runs. A surface whose entries are
    /// the machine's state of the moment (the processes running) or a
    /// catalogue the system searches by itself (the Store's apps, which
    /// desktop search reads from the shared catalogue) says `false`: its
    /// entries are found only while the app runs, and a closed app leaves
    /// none behind to open into something that is gone.
    fn kept_in_search_index(&self) -> bool {
        true
    }
}

/// Run `forward`, a pass of a surface over the document, keeping every change
/// it makes outside its dispatcher out of the document
/// ([`refuse_raw_writes`]).
fn only_through_actions(
    state: &mut ui_core::widget::Edits<'_>,
    path: &ModelPath,
    pass: &str,
    forward: impl FnOnce(&mut ui_core::widget::Edits<'_>, &ModelPath),
) {
    let before = ui_core::doc::document(state, path).0.pending_ops();
    forward(state, path);
    refuse_raw_writes(ui_core::doc::document(state, path).0, before, pass);
}

/// A surface changes its resource only through its dispatcher: every action
/// it applies is committed as one change, attributed and undoable. Anything
/// else would remain uncommitted; the typed loan prevents such a write.
/// A debug build also checks this invariant at the callback boundary.
fn refuse_raw_writes(doc: &AutoCommit, before: usize, pass: &str) {
    if before != 0 || doc.pending_ops() == 0 {
        return;
    }
    let dropped = doc.pending_ops();
    tracing::error!(
        pass,
        dropped,
        "a surface wrote the document outside an action; the write was refused"
    );
    debug_assert_eq!(
        dropped, 0,
        "a surface wrote the document outside an action during `{pass}`"
    );
}

/// What a hosted surface adds to its resource's publication, as the root
/// collects it.
pub(crate) trait SurfacePublication {
    /// The resource type the surface is hosted on; a root collects only its
    /// own type's surfaces.
    fn resource(&self) -> std::any::TypeId;
    /// Add the edits the surface binds to where the user is to `into`, a
    /// `Vec<ActionDecl<R>>` of its resource type `R`.
    fn bound_declarations(&self, into: &mut dyn std::any::Any);
    fn availability(&self) -> Vec<EffectStatus>;
    fn live_rows(&self) -> Vec<LiveRow>;
    fn live_row(&self, id: &str) -> Option<LiveRow>;
    /// Whether the surface binds edits on rows at all
    /// ([`ResourceSurface::ROW_DECLARATIONS`]).
    fn binds_rows(&self) -> bool;
    /// Whether the surface binds any edit on the row `row`.
    fn offers_on_row(&self, row: &str) -> bool;
    /// Add the edits the surface binds on the row `row` to `into`, a
    /// `Vec<ActionDecl<R>>` of its resource type `R`; whether it bound any.
    fn row_declarations(&self, row: &str, into: &mut dyn std::any::Any) -> bool;
    fn search_entries(&self) -> Vec<SearchEntry>;
    fn secrets_unlocked(&self) -> Option<bool>;
    fn kept_in_search_index(&self) -> bool;
    fn supports_device_shell(&self) -> bool;
    fn device_shell_operation(
        &mut self,
        operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String>;
    fn request_device_shell(&mut self) -> bool;
    fn search_queries(&self) -> SearchQueries;
    fn take_host_intent(&mut self) -> Option<ui_core::widget::AppHostIntent>;
    fn take_save_retry(&mut self) -> bool;
}

/// Coalesced query delivery owned by the canonical provider and surface host.
#[derive(Clone, Default)]
pub(crate) struct SearchQueries(Rc<RefCell<QueryDelivery>>);

#[derive(Default)]
struct QueryDelivery {
    last: Option<ui_core::search::SearchQuery>,
    pending: Option<ui_core::search::SearchQuery>,
    /// The surface said its results for `last` are still coming.
    awaiting: bool,
    /// Wakes the window, so a query asked between frames reaches the
    /// surface without waiting for input. `None` where no query is ever
    /// delivered (History).
    wake: Option<WakeHandle>,
}

impl SearchQueries {
    fn waking(wake: Option<WakeHandle>) -> Self {
        Self(Rc::new(RefCell::new(QueryDelivery {
            wake,
            ..QueryDelivery::default()
        })))
    }

    /// Whether the surface has yet to answer the latest query: it has not
    /// been given it, or said its results are still coming.
    pub(crate) fn answering(&self) -> bool {
        let delivery = self.0.borrow();
        delivery.wake.is_some() && (delivery.pending.is_some() || delivery.awaiting)
    }

    /// The latest query asked.
    pub(crate) fn last(&self) -> Option<ui_core::search::SearchQuery> {
        self.0.borrow().last.clone()
    }

    /// What the surface says of its results for the latest query.
    fn settle(&self, awaiting: bool) {
        self.0.borrow_mut().awaiting = awaiting;
    }

    pub(crate) fn submit(&self, query: &ui_core::search::SearchQuery) {
        if query
            .text()
            .chars()
            .nth(ui_core::search::MAX_SEARCH_QUERY_SCALARS)
            .is_some()
        {
            return;
        }
        let wake = {
            let mut delivery = self.0.borrow_mut();
            if delivery.last.as_ref() == Some(query) {
                return;
            }
            delivery.last = Some(query.clone());
            delivery.pending = Some(query.clone());
            delivery.awaiting = false;
            delivery.wake.clone()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    fn take(&self) -> Option<ui_core::search::SearchQuery> {
        self.0.borrow_mut().pending.take()
    }
}

/// What a surface receives when attached.
pub struct SurfaceCx<R: Resource> {
    pub(crate) env: super::env::RecordEnv<R>,
    pub(crate) jobs: Jobs<R>,
    pub(crate) actions: Actions<R>,
    pub(crate) stages: super::stage::Stages,
    pub(crate) scope: Option<Rc<SurfaceScope>>,
}

impl<R: Resource> Clone for SurfaceCx<R> {
    fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
            jobs: self.jobs.clone(),
            actions: self.actions.clone(),
            stages: self.stages.clone(),
            scope: self.scope.clone(),
        }
    }
}

impl<R: Resource> std::ops::Deref for SurfaceCx<R> {
    type Target = super::env::RecordEnv<R>;
    fn deref(&self) -> &super::env::RecordEnv<R> {
        &self.env
    }
}

impl<R: Resource> SurfaceCx<R> {
    /// Host a dynamically prepared child through the same resource, dispatcher
    /// and effect publication as this surface. Removing either host retires its
    /// effects and jobs, even when somebody retains its context or child widget.
    ///
    /// # Errors
    /// Refuses a child after this surface's owning view has been retired.
    pub fn surface<S, E>(&self, surface: S) -> Result<ui_core::widget::Element, &'static str>
    where
        S: ResourceSurface<R, E> + 'static,
        E: Effect,
    {
        if self.alive.strong_count() == 0
            || self.scope.as_ref().is_some_and(|scope| !scope.active())
        {
            return Err("the parent surface has been retired");
        }
        Ok(self.host_surface(surface))
    }

    pub(crate) fn host_surface<S, E>(&self, mut surface: S) -> ui_core::widget::Element
    where
        S: ResourceSurface<R, E> + 'static,
        E: Effect,
    {
        let scope = SurfaceScope::under(self.alive.clone(), self.scope.as_ref());
        let jobs = self.effects.scoped_surface_jobs(&scope);
        let mut cx = self.clone();
        cx.jobs = jobs.clone();
        cx.env.alive = scope.alive();
        cx.scope = Some(Rc::clone(&scope));
        surface.attach(cx);
        surface.present(&self.live.get());
        let host = SurfaceHost::new(
            self.live.clone(),
            self.dispatch.clone(),
            &self.effects,
            jobs,
            Rc::clone(&scope),
            surface,
        );
        stage_hosted(&self.stages, scope, host.staging());
        HostedSurface::element(Box::new(host))
    }
    /// The host-owned identity of the record this surface shows. A durable
    /// activation link can name it without relying on the last-open window.
    /// Absent in a historical view that cannot launch live work.
    pub fn record(&self) -> Option<&ui_core::record::RecordId> {
        self.run.launched.record.as_ref()
    }

    /// What [`RecordEnv::open_here`](super::env::RecordEnv::open_here) does, kept for later: the input that asks
    /// for another record arrives after attaching (a file chosen in a
    /// picker). `None` in a past version and a capture.
    pub fn opener(&self) -> Option<super::OpenHere> {
        self.run.opener.clone()
    }

    /// Jobs the surface starts on its own, with nobody invoking an effect;
    /// their completions get `&mut` the surface (`jobs.start::<Self>(…)`).
    /// Keep the handle. See [`Jobs`].
    pub fn jobs(&self) -> Jobs<R> {
        self.jobs.clone()
    }

    /// The resource's published actions and the surface's effects, for
    /// binding the toolkit controls a surface builds itself (a toggle, a
    /// dropdown, a button deep in its own widget tree) the way a view's
    /// controls are bound: [`Actions::bind`], [`Actions::bind_effect`].
    pub fn actions(&self) -> Actions<R> {
        self.actions.clone()
    }

    /// The live state, for a surface that reads it where it needs it (in
    /// `draw`, in an event) rather than copying what it was presented, and
    /// that asks for the parts of a large resource it shows
    /// ([`Live::demand`]).
    pub fn live(&self) -> Live<R> {
        self.live.clone()
    }

    /// The file whose history History shows beside the resource's, for an
    /// app that declares [`ResourceApp::FILE_HISTORY`](super::ResourceApp::FILE_HISTORY):
    /// set its target to the file the surface shows (the selected one) and
    /// open it from the surface's own History command. `None` for any other
    /// app, and in a past version.
    pub fn file_history(&self) -> Option<ui_core::history_mode::checked_files::FileHistorySource> {
        self.run
            .file_history
            .as_ref()
            .map(|(source, _)| source.clone())
    }

    /// The id of the app the surface is part of, which its presence keys
    /// name ([`ResourceSelection::presence`]).
    pub fn app_id(&self) -> &'static str {
        self.dispatch.app_id()
    }

    /// The app's secrets. A secret of one document item (a profile's
    /// password) is `cx.secrets().item(purpose, id)`.
    pub fn secrets(&self) -> ui_core::secrets::SecretStore {
        self.storage.storage().secrets()
    }

    /// The app's private file areas.
    pub fn files(&self) -> ui_core::storage::AppFiles {
        self.storage.storage().files()
    }

    /// Wakes the app from another thread — a worker's result, a socket's
    /// message — so the surface syncs and draws it without waiting for
    /// input.
    pub fn waker(&self) -> WakeHandle {
        self.effects
            .env()
            .map_or_else(WakeHandle::new, |env| env.wake.clone())
    }
}

/// Where a participant is inside a resource, in the resource's own terms, so
/// it means the same thing on every collaborator's screen.
#[derive(Clone, Debug, PartialEq)]
pub enum ResourceSelection {
    /// An item (a clip, a card, a row) is selected.
    Item(String),
    /// A caret at a position inside an item (a text field, a cell).
    Caret { item: String, at: usize },
    /// A range inside an item: characters of a field, frames of a clip.
    Range {
        item: String,
        start: usize,
        end: usize,
    },
    /// A pointer over an item, at a point of the shared window (logical
    /// pixels): where a collaborator is aiming in a surface that draws
    /// their cursor itself.
    Pointer { item: String, x: f32, y: f32 },
}

impl ResourceSelection {
    /// The toolkit presence frame for this selection, keyed to the resource
    /// item so peers resolve it against their own layout.
    pub fn presence<R: Resource>(&self, app_id: &str) -> (PresenceKey, PresencePayload) {
        self.presence_of(R::KIND, app_id)
    }

    /// [`Self::presence`] for a resource known by its `kind` alone.
    pub fn presence_of(&self, kind: &str, app_id: &str) -> (PresenceKey, PresencePayload) {
        let key = |item: &str| PresenceKey::Resource {
            app: app_id.to_owned(),
            id: item.to_owned(),
            kind: kind.to_owned(),
        };
        match self {
            Self::Item(item) => (key(item), PresencePayload::Selection { start: 0, end: 0 }),
            Self::Caret { item, at } => (key(item), PresencePayload::Caret { pos: *at }),
            Self::Range { item, start, end } => (
                key(item),
                PresencePayload::Selection {
                    start: *start,
                    end: *end,
                },
            ),
            Self::Pointer { item, x, y } => (key(item), PresencePayload::Pointer { x: *x, y: *y }),
        }
    }
}

impl From<tilo_resource::Place> for ResourceSelection {
    fn from(place: tilo_resource::Place) -> Self {
        match place {
            tilo_resource::Place::Item(item) => Self::Item(item),
            tilo_resource::Place::Caret { item, at } => Self::Caret { item, at },
            tilo_resource::Place::Range { item, start, end } => Self::Range { item, start, end },
        }
    }
}

/// Presents resource changes to a hosted surface, routes its effects to it,
/// and otherwise stays out of its way: layout, drawing and input are the
/// surface's own.
///
/// The surface is laid out first, so the host and the surface it forwards
/// its name to are one address: the window's map of named widgets, which
/// keys a widget by address and name, finds the surface's own node when a
/// test or a headless drive names it.
#[repr(C)]
pub(crate) struct SurfaceHost<R: Resource, S, E> {
    surface: S,
    search_queries: SearchQueries,
    live: Live<R>,
    /// The live revision and loads ([`Live::loads`]) of the state last
    /// presented.
    presented: (u64, u64),
    /// The number of the next landing to tell the surface of.
    landings: u64,
    /// The number of the next action applied here to tell the surface of,
    /// when it follows them ([`ResourceSurface::APPLIED`]).
    applied: Option<u64>,
    dispatch: Dispatcher<R>,
    read_only: bool,
    /// A scenario to stage on the next sync.
    staging: Staging,
    /// Runs the surface's effects; `None` in History mode, where they never
    /// run.
    effects: Option<EffectRunner<R, S, E>>,
    /// What the surface last said of its effects, which the root publishes.
    statuses: Rc<RefCell<Vec<EffectStatus>>>,
    /// The jobs the surface started on its own.
    jobs: OwnedJobs<R, S>,
    scope: Rc<SurfaceScope>,
}

impl<R: Resource, S, E> Drop for SurfaceHost<R, S, E> {
    fn drop(&mut self) {
        self.scope.retire();
        if self.applied.is_some() {
            self.live.unfollow_applied();
        }
    }
}

/// The scenario a surface stages on its next sync: outer `None` is nothing
/// to stage, inner `None` is a plain `--shots`.
pub(crate) type Staging = Rc<RefCell<Option<Option<String>>>>;

impl<R: Resource, S: ResourceSurface<R, E> + 'static, E: Effect> SurfaceHost<R, S, E> {
    pub(crate) fn new(
        live: Live<R>,
        dispatch: Dispatcher<R>,
        effects: &Effects<R>,
        jobs: Jobs<R>,
        scope: Rc<SurfaceScope>,
        surface: S,
    ) -> Self {
        let statuses = Rc::new(RefCell::new(surface.effects()));
        let runner = effects
            .env()
            .map(|env| EffectRunner::new(env.scoped(&scope)));
        let search_queries = SearchQueries::waking(effects.env().map(|env| env.wake.clone()));
        // Captures and History describe their controls too. Publication is
        // inert metadata; only the runner below can execute an effect.
        if !E::declarations().is_empty() {
            let published = Rc::clone(&statuses);
            let lifetime = Rc::clone(&scope);
            let kept = super::effect::KeptSpecs::default();
            effects.publish::<E>(scope.alive(), move || {
                if lifetime.active() {
                    super::effect::kept_specs::<E>(&kept, &published.borrow())
                } else {
                    Vec::new()
                }
            });
        }
        Self {
            presented: (live.revision(), live.loads()),
            landings: live.next_landing(),
            applied: S::APPLIED.then(|| live.follow_applied()),
            live,
            read_only: dispatch.read_only(),
            dispatch,
            staging: Staging::default(),
            effects: runner,
            statuses,
            jobs: OwnedJobs::new(jobs),
            surface,
            search_queries,
            scope,
        }
    }

    pub(crate) fn staging(&self) -> Staging {
        Rc::clone(&self.staging)
    }
}

/// What a [`HostedSurface`] asks of the host of a surface of particular
/// types: the only surface-hosting code compiled per surface.
pub(crate) trait SurfaceDriver: SurfacePublication {
    /// The surface, as a widget.
    fn surface(&self) -> &(dyn Widget + 'static);
    fn surface_mut(&mut self) -> &mut (dyn Widget + 'static);
    /// The surface by its own type, for whoever reaches it so (a test
    /// driving it, a host that embeds it).
    fn surface_any(&self) -> &dyn std::any::Any;
    fn surface_any_mut(&mut self) -> &mut dyn std::any::Any;
    /// Whether `event` only scrolls the view, with nothing for the host to
    /// present first.
    fn view_only_scroll(&self, event: &ui_core::winit::event::WindowEvent) -> bool;
    /// Present what changed, then sync the surface.
    fn sync(&mut self, state: &AppModel, path: &ModelPath);
    /// Start `op` as one of the surface's effects, if it is one.
    ///
    /// # Errors
    /// The effect's refusal.
    fn start_effect(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &AutoCommit,
    ) -> Result<Option<ResourceOperationStart>, String>;
    /// Progress of one of the surface's effects, if `invocation` is one.
    fn poll_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
    ) -> Option<tilo_actions::InvocationUpdate>;
}

impl<R: Resource, S: ResourceSurface<R, E> + 'static, E: Effect> SurfaceDriver
    for SurfaceHost<R, S, E>
{
    fn surface(&self) -> &(dyn Widget + 'static) {
        &self.surface
    }

    fn surface_mut(&mut self) -> &mut (dyn Widget + 'static) {
        &mut self.surface
    }

    fn surface_any(&self) -> &dyn std::any::Any {
        &self.surface
    }

    fn surface_any_mut(&mut self) -> &mut dyn std::any::Any {
        &mut self.surface
    }

    fn view_only_scroll(&self, event: &ui_core::winit::event::WindowEvent) -> bool {
        (self.live.revision(), self.live.loads()) == self.presented
            && self.staging.borrow().is_none()
            && self.search_queries.0.borrow().pending.is_none()
            && self.dispatch.read_only() == self.read_only
            && self.surface.view_only_scroll(event)
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if !self.scope.active() {
            self.effects = None;
            return;
        }
        let read_only = self.dispatch.read_only();
        if read_only != self.read_only {
            self.read_only = read_only;
            self.surface.read_only_changed(read_only);
        }
        let held = (self.live.revision(), self.live.loads());
        if held != self.presented {
            self.presented = held;
            self.surface.present(&self.live.get());
        }
        let (landed, next) = self.live.landings_since(self.landings);
        self.landings = next;
        for (participant, place) in &landed {
            self.surface.landed(participant, place);
        }
        if let Some(from) = self.applied {
            let (applied, next) = self.live.applied_since(from);
            self.applied = Some(next);
            for action in &applied {
                self.surface.applied(action);
            }
        }
        let staged = self.staging.borrow_mut().take();
        if let Some(scenario) = staged {
            self.surface.stage(scenario.as_deref());
        }
        if let Some(query) = self.search_queries.take()
            && self.effects.is_some()
        {
            self.surface.search_query(&query);
        }
        self.jobs.poll(&mut self.surface);
        self.surface.sync(state, path);
        self.search_queries
            .settle(self.effects.is_some() && self.surface.search_pending());
        if self.effects.is_some() {
            note_statuses(&self.statuses, self.surface.effects());
        }
    }
    fn start_effect(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &AutoCommit,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if !self.scope.active() {
            return Err("the surface has been retired".into());
        }
        match self.effects.as_mut() {
            Some(runner) => runner.start(op, &mut self.surface, Some(doc), S::effect),
            None => Ok(None),
        }
    }

    fn poll_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if !self.scope.active() {
            self.effects = None;
            return None;
        }
        self.effects
            .as_mut()?
            .poll(invocation, cancellation_requested, &mut self.surface)
    }
}

/// A hosted surface as mounted in the view: its [`SurfaceHost`] behind the
/// one widget type every surface shares.
pub(crate) struct HostedSurface {
    host: Box<dyn SurfaceDriver>,
    /// The empty states the surface painted itself, described for it.
    painted: ui_core::widget::empty_state::PaintedEmptyStates,
}

impl HostedSurface {
    /// `host` mounted as a widget. The widget is made here, outside any
    /// generic code, so its implementation of the widget contract is
    /// compiled once in the shared runtime rather than in every app.
    #[inline(never)]
    pub(crate) fn element(host: Box<dyn SurfaceDriver>) -> ui_core::widget::Element {
        Box::new(Self {
            host,
            painted: ui_core::widget::empty_state::PaintedEmptyStates::default(),
        })
    }
}

// The host adds four things to the surface — presenting new resource state
// before the surface syncs, answering its effects, publishing what it says
// of them, and describing the empty states it paints itself — and forwards
// everything else to it.
impl Decorator for HostedSurface {
    fn provide<'a>(&'a self, request: &mut ui_core::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn SurfacePublication>() {
            request.provide::<dyn SurfacePublication>(self);
            return;
        }
        if request.wants::<dyn ui_core::resources::PublishesResources>() {
            request.provide::<dyn ui_core::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ResourceOperations>() {
            request.provide::<dyn ResourceOperations>(self);
            return;
        }
        self.host.surface().provide(request);
    }

    fn provide_mut<'a>(
        &'a mut self,
        request: &mut ui_core::widget::capability::RequestMut<'a, '_>,
    ) {
        if request.wants::<dyn SurfacePublication>() {
            request.provide::<dyn SurfacePublication>(self);
            return;
        }
        if request.wants::<dyn ui_core::resources::PublishesResources>() {
            request.provide::<dyn ui_core::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ResourceOperations>() {
            request.provide::<dyn ResourceOperations>(self);
            return;
        }
        self.host.surface_mut().provide_mut(request);
    }

    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.host.surface()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.host.surface_mut()
    }

    fn view_only_scroll(&self, event: &ui_core::winit::event::WindowEvent) -> bool {
        self.host.view_only_scroll(event)
    }

    fn draw(
        &mut self,
        scene: &mut ui_core::vello::Scene,
        transform: ui_core::vello::kurbo::Affine,
        text_renderer: &mut ui_core::TextRenderer,
        theme: &ui_core::Theme,
        presence: &ui_core::PresenceCtx,
    ) {
        let surface = self.host.surface_mut();
        self.painted.draw(transform, || {
            surface.draw(scene, transform, text_renderer, theme, presence);
        });
    }

    // An empty state drawn as pixels is described as well, its action a
    // button that clicks where it is drawn: often the only way on from a
    // first-use screen, for a screen reader as much as for anyone.
    fn accessibility(&self) -> Option<ui_core::a11y::AccessNode> {
        let surface = self.host.surface();
        self.painted.describe(surface.accessibility(), surface)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<ui_core::vello::kurbo::Rect> {
        Decorator::accessibility(self)
            .and_then(|described| self.painted.action_rect(&described, path))
            .or_else(|| self.host.surface().synthetic_accessibility_rect(path))
    }

    // The host adds no structure of its own: a change its surface can
    // describe stays described.
    fn take_structure_change(&mut self) -> ui_core::widget::StructureChange {
        self.host.surface_mut().take_structure_change()
    }

    // The host is invisible to whoever reaches the surface by its type (a
    // test driving it, a host that embeds it): it answers as the surface.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self.host.surface_any())
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self.host.surface_any_mut())
    }

    fn settle(&mut self, edits: &mut ui_core::widget::Edits<'_>, path: &ObjId) {
        self.host.surface_mut().settle(edits, path);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.host.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.host.surface_mut().update(state, path)
    }

    fn handle_event(
        &mut self,
        event: &ui_core::winit::event::WindowEvent,
        ctx: &mut ui_core::widget::EventCtx<'_>,
    ) {
        let before = ctx.state.pending_ops();
        self.host.surface_mut().handle_event(event, ctx);
        refuse_raw_writes(ctx.state.doc(), before, "handle_event");
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui_core::a11y::Action,
        state: &mut ui_core::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let surface = self.host.surface_mut();
        let mut performed = false;
        only_through_actions(state, path, "perform_semantic_action", |state, path| {
            performed = surface.perform_semantic_action(target, action, state, path);
        });
        performed
    }
}

impl ResourceOperations for HostedSurface {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        ui_core::host_operation::resource_operation_handlers(self.host.surface())
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if let Some(started) = self.host.start_effect(op, doc.doc())? {
            return Ok(Some(started));
        }
        // The surface answers its operations as its effect vocabulary; only
        // the toolkit's own widgets inside it (a button bound to a view)
        // answer theirs, never the surface by hand. A toolkit container the
        // surface decorates (its shell) routes them to its parts, those its
        // children do not list among them too (a header button shown in
        // the window's bar), as it lists their handlers.
        let before = doc.pending_ops();
        let surface = self.host.surface_mut();
        let started = if decorates_routing(surface) {
            ui_core::host_operation::start_operation(surface, op, doc, path)
        } else {
            ui_core::host_operation::start_in_children(surface, op, doc, path)
        };
        refuse_raw_writes(doc, before, "start_operation");
        started
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if let Some(update) = self.host.poll_effect(invocation, cancellation_requested) {
            return Some(update);
        }
        let surface = self.host.surface_mut();
        if decorates_routing(surface) {
            return ui_core::host_operation::poll_resource_operation(
                surface,
                invocation,
                cancellation_requested,
                doc,
                path,
            );
        }
        surface.children_mut().into_iter().find_map(|child| {
            ui_core::host_operation::poll_resource_operation(
                child,
                invocation,
                cancellation_requested,
                doc,
                path,
            )
        })
    }
}

/// Whether `surface` decorates a toolkit container that routes operations
/// itself ([`ResourceOperations`]), rather than leaving them to its
/// children.
fn decorates_routing(surface: &(impl Widget + ?Sized)) -> bool {
    ui_core::widget::capability::capability::<dyn ResourceOperations>(surface).is_some()
}

impl SurfacePublication for HostedSurface {
    fn resource(&self) -> std::any::TypeId {
        self.host.resource()
    }
    fn bound_declarations(&self, into: &mut dyn std::any::Any) {
        self.host.bound_declarations(into);
    }
    fn availability(&self) -> Vec<EffectStatus> {
        self.host.availability()
    }
    fn live_rows(&self) -> Vec<LiveRow> {
        self.host.live_rows()
    }
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.host.live_row(id)
    }
    fn binds_rows(&self) -> bool {
        self.host.binds_rows()
    }
    fn offers_on_row(&self, row: &str) -> bool {
        self.host.offers_on_row(row)
    }
    fn row_declarations(&self, row: &str, into: &mut dyn std::any::Any) -> bool {
        self.host.row_declarations(row, into)
    }
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.host.search_entries()
    }
    fn secrets_unlocked(&self) -> Option<bool> {
        self.host.secrets_unlocked()
    }
    fn kept_in_search_index(&self) -> bool {
        self.host.kept_in_search_index()
    }
    fn supports_device_shell(&self) -> bool {
        self.host.supports_device_shell()
    }
    fn device_shell_operation(
        &mut self,
        operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        self.host.device_shell_operation(operation)
    }
    fn request_device_shell(&mut self) -> bool {
        self.host.request_device_shell()
    }
    fn search_queries(&self) -> SearchQueries {
        self.host.search_queries()
    }
    fn take_host_intent(&mut self) -> Option<ui_core::widget::AppHostIntent> {
        self.host.take_host_intent()
    }
    fn take_save_retry(&mut self) -> bool {
        self.host.take_save_retry()
    }
}

impl<R: Resource, S: ResourceSurface<R, E> + 'static, E: Effect> SurfacePublication
    for SurfaceHost<R, S, E>
{
    fn resource(&self) -> std::any::TypeId {
        std::any::TypeId::of::<R>()
    }

    fn supports_device_shell(&self) -> bool {
        self.surface.supports_device_shell()
    }
    fn device_shell_operation(
        &mut self,
        operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        self.surface.device_shell_operation(operation)
    }
    fn request_device_shell(&mut self) -> bool {
        self.surface.request_device_shell()
    }

    fn take_save_retry(&mut self) -> bool {
        self.effects.is_some() && self.surface.take_save_retry()
    }

    fn take_host_intent(&mut self) -> Option<ui_core::widget::AppHostIntent> {
        self.surface.take_host_intent()
    }

    fn bound_declarations(&self, into: &mut dyn std::any::Any) {
        if let Some(into) = into.downcast_mut::<Vec<ActionDecl<R>>>() {
            into.extend(self.surface.bound_declarations());
        }
    }

    fn availability(&self) -> Vec<EffectStatus> {
        self.surface.availability()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.surface.live_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.surface.live_row(id)
    }

    fn binds_rows(&self) -> bool {
        S::ROW_DECLARATIONS
    }

    fn offers_on_row(&self, row: &str) -> bool {
        self.binds_rows() && !self.surface.row_declarations(row).is_empty()
    }

    fn row_declarations(&self, row: &str, into: &mut dyn std::any::Any) -> bool {
        if !self.binds_rows() {
            debug_assert!(
                self.surface.row_declarations(row).is_empty(),
                "a surface binding edits on rows sets `ResourceSurface::ROW_DECLARATIONS`"
            );
            return false;
        }
        let offered = self.surface.row_declarations(row);
        match into.downcast_mut::<Vec<ActionDecl<R>>>() {
            Some(into) if !offered.is_empty() => {
                into.extend(offered);
                true
            }
            _ => false,
        }
    }

    fn search_entries(&self) -> Vec<SearchEntry> {
        self.surface.search_entries()
    }

    fn secrets_unlocked(&self) -> Option<bool> {
        self.surface.secrets_unlocked()
    }

    fn kept_in_search_index(&self) -> bool {
        self.surface.kept_in_search_index()
    }

    fn search_queries(&self) -> SearchQueries {
        self.search_queries.clone()
    }
}

// What the surface publishes as a widget passes through here, without
// document actions: those are the resource's, through its dispatcher.
impl ui_core::resources::PublishesResources for HostedSurface {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        ui_core::resources::resources(self.host.surface(), doc)
            .into_iter()
            .map(outside_document)
            .collect()
    }

    fn caller_view_resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        ui_core::resources::caller_view_resources(self.host.surface(), doc)
            .into_iter()
            .map(outside_document)
            .collect()
    }

    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui_core::widget::ActionDirectoryResourcePage> {
        let mut page = ui_core::resources::action_directory_resource_page(
            self.host.surface(),
            doc,
            cursor,
            max_results,
        )?;
        page.resources = page.resources.into_iter().map(outside_document).collect();
        Some(page)
    }

    fn action_directory_revision(&self, doc: &AutoCommit) -> Option<String> {
        ui_core::resources::action_directory_revision(self.host.surface(), doc)
    }

    fn action_directory_invocation_domain(
        &self,
        doc: &AutoCommit,
    ) -> ui_core::widget::ActionDirectoryInvocationDomain {
        ui_core::resources::action_directory_invocation_domain(self.host.surface(), doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        ui_core::resources::resource(self.host.surface(), doc, id).map(resolved_outside_document)
    }

    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        ui_core::resources::resources_for(self.host.surface(), participant, doc)
            .into_iter()
            .map(outside_document)
            .collect()
    }

    fn resource_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        ui_core::resources::resource_for(self.host.surface(), participant, doc, id)
            .map(resolved_outside_document)
    }

    fn action_directory_visibility(
        &self,
        doc: &AutoCommit,
        resource_id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        ui_core::resources::action_directory_visibility(self.host.surface(), doc, resource_id)
    }

    fn document_action_result(
        &self,
        doc: &AutoCommit,
        address: &tilo_actions::ActionAddress,
        changes: &[ui_core::automerge::Patch],
    ) -> Option<serde_json::Value> {
        ui_core::resources::document_action_result(self.host.surface(), doc, address, changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_core::automerge::ROOT;
    use ui_core::automerge::transaction::Transactable;

    #[test]
    fn what_a_surface_applied_through_actions_stays() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "title", "applied").unwrap();
        doc.commit();
        let heads = doc.get_heads();
        refuse_raw_writes(&doc, 0, "sync");
        assert_eq!(doc.get_heads(), heads);
    }

    #[test]
    #[should_panic(expected = "outside an action")]
    fn a_surface_writing_around_its_dispatcher_is_stopped() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "title", "raw").unwrap();
        refuse_raw_writes(&doc, 0, "sync");
    }
}
