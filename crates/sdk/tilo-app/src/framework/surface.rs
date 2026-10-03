//! The contract a custom widget or renderer implements to take part in a
//! resource: collaboration, history, undo, permissions and the action door.

use std::cell::RefCell;
use std::hash::Hash;
use std::rc::{Rc, Weak};
use ui_core::widget::{AppModel, ModelPath};

use tilo_resource::{ActionDecl, Effect, NoEffect, Resource};
use ui_core::automerge::{AutoCommit, ObjId};
use ui_core::presence::PresenceKey;
use ui_core::widget::{Decorator, ResourceOperation, ResourceOperationStart, Widget};
use ui_core::{PresencePayload, WakeHandle};

use super::dispatch::Dispatcher;
use super::effect::{EffectAnswer, EffectCx, EffectRunner, EffectStatus, Effects};
use super::live::Live;
use super::rows::{LiveRow, outside_document, resolved_outside_document};
use super::scenario::{Run, RunMode};
use super::settings::{Setting, ViewStorage};

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
/// * **Permissions** — [`SurfaceCx::read_only`] says the participant may
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
///   service, a socket, a probe) only when [`SurfaceCx::mode`] is live. A
///   capture reads [`SurfaceCx::fixture`] instead; a past version starts
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
    fn present(&mut self, state: &Rc<R::State>);

    /// The channel edits go through, and whether they will be accepted.
    /// Called once, before the first [`present`](Self::present).
    fn attach(&mut self, cx: SurfaceCx<R>) {
        let _ = cx;
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

    /// Edits bound to the surface's view state — the selection, the caret,
    /// the playhead, the folder or page shown — as typed builders of the
    /// resource's action, published on the resource beside its declared
    /// actions. Invoking one dispatches the action it builds through the
    /// same permission check, confirmation and undo as any other. An id the
    /// resource already publishes keeps the resource's action.
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

    /// Whether the document actions this surface's own `PublishesResources`
    /// offers (edits bound to its selection, published as hand-built rows)
    /// still reach the published tree. Otherwise they are dropped, and a
    /// debug build stops at the first one. The surfaces that answer `true`
    /// are listed in the policy baseline, which only shrinks: their edits
    /// move to [`bound_declarations`](Self::bound_declarations).
    fn publishes_document_actions(&self) -> bool {
        false
    }
}

/// What a hosted surface adds to its resource's publication, as the root
/// collects it.
pub(crate) trait SurfacePublication<R: Resource> {
    fn bound_declarations(&self) -> Vec<ActionDecl<R>>;
    fn live_rows(&self) -> Vec<LiveRow>;
    fn live_row(&self, id: &str) -> Option<LiveRow>;
}

impl<R: Resource, S: ResourceSurface<R, E>, E: Effect> SurfaceHost<R, S, E> {
    /// `resource` as the surface may publish it beside the resource.
    fn admitted(&self, resource: tilo_actions::Resource) -> tilo_actions::Resource {
        if self.surface.publishes_document_actions() {
            resource
        } else {
            outside_document(resource)
        }
    }

    /// `resource`, resolved by id, as the surface may answer it.
    fn resolved(&self, resource: tilo_actions::Resource) -> tilo_actions::Resource {
        if self.surface.publishes_document_actions() {
            resource
        } else {
            resolved_outside_document(resource)
        }
    }
}

/// What a surface receives when attached.
pub struct SurfaceCx<R: Resource> {
    pub(crate) dispatch: Dispatcher<R>,
    pub(crate) live: Live<R>,
    pub(crate) run: Run,
    pub(crate) storage: Rc<ViewStorage>,
    pub(crate) effects: Effects<R>,
    pub(crate) alive: Weak<()>,
}

impl<R: Resource> SurfaceCx<R> {
    pub fn dispatcher(&self) -> Dispatcher<R> {
        self.dispatch.clone()
    }

    /// The live state, for a surface that reads it where it needs it (in
    /// `draw`, in an event) rather than copying what it was presented, and
    /// that asks for the parts of a large resource it shows
    /// ([`Live::demand`]).
    pub fn live(&self) -> Live<R> {
        self.live.clone()
    }

    /// Who the surface is for: the participant reading and editing through
    /// it, the same id an action they invoke reads as its author. See
    /// [`ViewCx::me`](super::ViewCx::me). `None` when the surface shows a
    /// past version.
    pub fn me(&self) -> Option<&str> {
        self.dispatch.me()
    }

    /// Whether the participant may only view, as of now.
    pub fn read_only(&self) -> bool {
        self.dispatch.read_only()
    }

    /// What the surface is built for. Start work that reaches outside the
    /// process (a decoder service, a socket, a hardware probe) only when it
    /// [is live](RunMode::is_live); a capture reads its
    /// [fixtures](Self::fixture) instead, and a past version starts nothing.
    pub fn mode(&self) -> RunMode {
        self.run.mode
    }

    /// The capture's stand-in for a live source of type `T`
    /// ([`Fixture::provide`](super::Fixture::provide)); `None` outside a
    /// capture, or when the scenario provides none.
    pub fn fixture<T: 'static>(&self) -> Option<Rc<T>> {
        self.run.fixture::<T>()
    }

    /// The content store the host lent the app. A capture's holds the blobs
    /// its scenario put there.
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        self.run.content()
    }

    /// The id of the app the surface is part of, which its presence keys
    /// name ([`ResourceSelection::presence`]).
    pub fn app_id(&self) -> &'static str {
        self.dispatch.app_id()
    }

    /// The app's typed settings of type `S`, kept current while the view
    /// that hosts the surface lives: keep the [`Setting`] and read it where
    /// it is needed. `S` declares whether they follow the person or stay on
    /// the device ([`Scope`](ui_core::storage::Scope)).
    pub fn settings<S>(&self) -> Setting<S>
    where
        S: ui_core::storage::SettingsSchema + Clone + PartialEq + Send + Sync,
    {
        self.storage.follow(self.alive.clone())
    }

    /// The app's files, secrets and settings documents, as its host gave
    /// them.
    pub fn storage(&self) -> ui_core::storage::AppStorage {
        self.storage.storage().clone()
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

    /// Drain a projection the resource's actions queue, as
    /// [`ViewCx::drain`](super::ViewCx::drain).
    pub fn drain<K, T>(
        &self,
        pending: impl Fn(&R::State) -> Vec<(K, T)> + 'static,
        run: impl FnMut(T, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) where
        K: Eq + Hash + Clone + 'static,
        T: 'static,
    {
        self.effects.drain(self.alive.clone(), pending, run);
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
}

impl ResourceSelection {
    /// The toolkit presence frame for this selection, keyed to the resource
    /// item so peers resolve it against their own layout.
    pub fn presence<R: Resource>(&self, app_id: &str) -> (PresenceKey, PresencePayload) {
        let key = |item: &str| PresenceKey::Resource {
            app: app_id.to_owned(),
            id: item.to_owned(),
            kind: R::KIND.to_owned(),
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
        }
    }
}

/// Presents resource changes to a hosted surface, routes its effects to it,
/// and otherwise stays out of its way: layout, drawing and input are the
/// surface's own.
pub(crate) struct SurfaceHost<R: Resource, S, E> {
    live: Live<R>,
    presented: u64,
    dispatch: Dispatcher<R>,
    read_only: bool,
    /// A scenario to stage on the next sync.
    staging: Staging,
    /// Runs the surface's effects; `None` in History mode, where they never
    /// run.
    effects: Option<EffectRunner<R, S, E>>,
    /// What the surface last said of its effects, which the root publishes.
    statuses: Rc<RefCell<Vec<EffectStatus>>>,
    surface: S,
}

/// The scenario a surface stages on its next sync: outer `None` is nothing
/// to stage, inner `None` is a plain `--shots`.
pub(crate) type Staging = Rc<RefCell<Option<Option<String>>>>;

impl<R: Resource, S: ResourceSurface<R, E> + 'static, E: Effect> SurfaceHost<R, S, E> {
    pub(crate) fn new(
        live: Live<R>,
        dispatch: Dispatcher<R>,
        effects: &Effects<R>,
        alive: Weak<()>,
        surface: S,
    ) -> Self {
        let statuses = Rc::new(RefCell::new(surface.effects()));
        let runner = effects.env().map(|env| EffectRunner::new(env.clone()));
        if runner.is_some() && !E::declarations().is_empty() {
            let published = Rc::clone(&statuses);
            effects.publish::<E>(alive, move || {
                super::effect::published_specs::<E>(&published.borrow())
            });
        }
        Self {
            presented: live.revision(),
            live,
            read_only: dispatch.read_only(),
            dispatch,
            staging: Staging::default(),
            effects: runner,
            statuses,
            surface,
        }
    }

    pub(crate) fn staging(&self) -> Staging {
        Rc::clone(&self.staging)
    }
}

// The host adds three things to the surface — presenting new resource state
// before the surface syncs, answering its effects, and publishing what it
// says of them — and forwards everything else to it.
impl<R: Resource, S: ResourceSurface<R, E> + 'static, E: Effect> Decorator
    for SurfaceHost<R, S, E>
{
    ui_core::provides!(dyn SurfacePublication<R>, dyn ui_core::resources::PublishesResources; inner = surface);

    type Inner = S;

    fn inner(&self) -> &S {
        &self.surface
    }

    fn inner_mut(&mut self) -> &mut S {
        &mut self.surface
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let read_only = self.dispatch.read_only();
        if read_only != self.read_only {
            self.read_only = read_only;
            self.surface.read_only_changed(read_only);
        }
        if self.live.revision() != self.presented {
            self.presented = self.live.revision();
            self.surface.present(&self.live.get());
        }
        let staged = self.staging.borrow_mut().take();
        if let Some(scenario) = staged {
            self.surface.stage(scenario.as_deref());
        }
        self.surface.sync(state, path);
        if self.effects.is_some() {
            let now = self.surface.effects();
            if *self.statuses.borrow() != now {
                *self.statuses.borrow_mut() = now;
            }
        }
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if let Some(runner) = self.effects.as_mut()
            && let Some(started) = runner.start(op, &mut self.surface, |surface, effect, cx| {
                surface.effect(effect, cx)
            })?
        {
            return Ok(Some(started));
        }
        self.surface.start_operation(op, doc, path)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if let Some(runner) = self.effects.as_mut()
            && let Some(update) = runner.poll(invocation, cancellation_requested, &mut self.surface)
        {
            return Some(update);
        }
        self.surface
            .poll_resource_operation(invocation, cancellation_requested, doc, path)
    }
}

impl<R: Resource, S: ResourceSurface<R, E> + 'static, E: Effect> SurfacePublication<R>
    for SurfaceHost<R, S, E>
{
    fn bound_declarations(&self) -> Vec<ActionDecl<R>> {
        self.surface.bound_declarations()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.surface.live_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.surface.live_row(id)
    }
}

// What the surface publishes as a widget passes through here, without
// document actions: those are the resource's, through its dispatcher. A
// surface still publishing its own says so
// (`ResourceSurface::publishes_document_actions`).
impl<R: Resource, S: ResourceSurface<R, E> + 'static, E: Effect>
    ui_core::resources::PublishesResources for SurfaceHost<R, S, E>
{
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        ui_core::resources::resources(&self.surface, doc)
            .into_iter()
            .map(|resource| self.admitted(resource))
            .collect()
    }

    fn caller_view_resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        ui_core::resources::caller_view_resources(&self.surface, doc)
            .into_iter()
            .map(|resource| self.admitted(resource))
            .collect()
    }

    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui_core::widget::ActionDirectoryResourcePage> {
        let mut page = ui_core::resources::action_directory_resource_page(
            &self.surface,
            doc,
            cursor,
            max_results,
        )?;
        page.resources = page
            .resources
            .into_iter()
            .map(|resource| self.admitted(resource))
            .collect();
        Some(page)
    }

    fn action_directory_revision(&self, doc: &AutoCommit) -> Option<String> {
        ui_core::resources::action_directory_revision(&self.surface, doc)
    }

    fn action_directory_invocation_domain(
        &self,
        doc: &AutoCommit,
    ) -> ui_core::widget::ActionDirectoryInvocationDomain {
        ui_core::resources::action_directory_invocation_domain(&self.surface, doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        ui_core::resources::resource(&self.surface, doc, id).map(|resource| self.resolved(resource))
    }

    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        ui_core::resources::resources_for(&self.surface, participant, doc)
            .into_iter()
            .map(|resource| self.admitted(resource))
            .collect()
    }

    fn resource_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        ui_core::resources::resource_for(&self.surface, participant, doc, id)
            .map(|resource| self.resolved(resource))
    }

    fn action_directory_visibility(
        &self,
        doc: &AutoCommit,
        resource_id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        ui_core::resources::action_directory_visibility(&self.surface, doc, resource_id)
    }

    fn document_action_result(
        &self,
        doc: &AutoCommit,
        address: &tilo_actions::ActionAddress,
        changes: &[ui_core::automerge::Patch],
    ) -> Option<serde_json::Value> {
        ui_core::resources::document_action_result(&self.surface, doc, address, changes)
    }
}
