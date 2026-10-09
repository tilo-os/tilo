//! The contract a custom widget or renderer implements to take part in a
//! resource: collaboration, history, undo, permissions and the action door.

use std::cell::RefCell;
use std::hash::Hash;
use std::rc::{Rc, Weak};
use ui_core::widget::{AppModel, ModelPath};

use tilo_resource::{ActionDecl, Effect, NoEffect, Resource, SearchEntry};
use ui_core::automerge::transaction::Transactable as _;
use ui_core::automerge::{AutoCommit, ObjId};
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart};
use ui_core::presence::PresenceKey;
use ui_core::widget::{Decorator, Widget};
use ui_core::{PresencePayload, WakeHandle};

use super::controls::Actions;
use super::dispatch::Dispatcher;
use super::effect::{
    EffectAnswer, EffectCx, EffectRunner, EffectStatus, Effects, Jobs, OwnedJobs, Projection,
};
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

    /// Edits of the resource bound to the live row `row` (rename *this*
    /// file): typed builders of the resource's action, published on that row
    /// and applied through the same permission check, confirmation and undo
    /// as the resource's own declarations. A row itself carries only view
    /// and effect operations; this is how it offers a document change.
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
pub(crate) trait SurfacePublication<R: Resource> {
    fn bound_declarations(&self) -> Vec<ActionDecl<R>>;
    fn availability(&self) -> Vec<EffectStatus>;
    fn live_rows(&self) -> Vec<LiveRow>;
    fn live_row(&self, id: &str) -> Option<LiveRow>;
    fn row_declarations(&self, row: &str) -> Vec<ActionDecl<R>>;
    fn search_entries(&self) -> Vec<SearchEntry>;
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
}

impl SearchQueries {
    pub(crate) fn submit(&self, query: &ui_core::search::SearchQuery) {
        if query
            .text()
            .chars()
            .nth(ui_core::search::MAX_SEARCH_QUERY_SCALARS)
            .is_some()
        {
            return;
        }
        let mut delivery = self.0.borrow_mut();
        if delivery.last.as_ref() != Some(query) {
            delivery.last = Some(query.clone());
            delivery.pending = Some(query.clone());
        }
    }

    fn take(&self) -> Option<ui_core::search::SearchQuery> {
        self.0.borrow_mut().pending.take()
    }
}

/// What a surface receives when attached.
pub struct SurfaceCx<R: Resource> {
    pub(crate) jobs: Jobs<R>,
    pub(crate) dispatch: Dispatcher<R>,
    pub(crate) actions: Actions<R>,
    pub(crate) live: Live<R>,
    pub(crate) run: Run,
    pub(crate) storage: Rc<ViewStorage>,
    pub(crate) effects: Effects<R>,
    pub(crate) saved: ui_core::LocalState<ui_core::record::RecordSaveStatus>,
    pub(crate) alive: Weak<()>,
}

impl<R: Resource> SurfaceCx<R> {
    /// The host-owned identity of the record this surface shows. A durable
    /// activation link can name it without relying on the last-open window.
    /// Absent in a historical view that cannot launch live work.
    pub fn record(&self) -> Option<&ui_core::record::RecordId> {
        self.run.launched.record.as_ref()
    }

    /// Open, in this window and in place of the record it shows, what a
    /// launch of the app naming `arguments` opens: the record
    /// [`ResourceApp::launch_record`](super::ResourceApp::launch_record)
    /// names, with its launch actions — the document a person chose with
    /// the app's own Open. The window's view is rebuilt for that record on
    /// the next frame. Refused in a past version and a capture.
    ///
    /// The outcome answers once the host opened it, or why it could not.
    ///
    /// # Errors
    /// Returns that the window cannot open another record.
    pub fn open_here(
        &self,
        arguments: &[String],
    ) -> Result<ui_core::application::ReplacementOutcome, String> {
        self.run.open_here(arguments)
    }

    /// What [`Self::open_here`] does, kept for later: the input that asks
    /// for another record arrives after attaching (a file chosen in a
    /// picker). `None` in a past version and a capture.
    pub fn opener(&self) -> Option<super::OpenHere> {
        self.run.opener.clone()
    }

    /// The names of the participants, for showing the ids actions record
    /// ([`People::name`](super::People::name)).
    pub fn people(&self) -> super::People {
        self.run.people.clone()
    }

    pub fn dispatcher(&self) -> Dispatcher<R> {
        self.dispatch.clone()
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

    /// The dispatcher of the resource nested in this one at `id`, whose
    /// edits are this resource's action `wrap(edit)`: the body of the item a
    /// surface is editing. See [`Dispatcher::child`].
    pub fn child<C: Resource>(
        &self,
        id: &str,
        wrap: impl Fn(C::Action) -> R::Action + 'static,
    ) -> Dispatcher<C> {
        self.dispatch.child(id, wrap)
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

    /// Whether the surface shows a past version in History mode
    /// ([`ViewCx::is_past`](super::ViewCx::is_past)).
    pub fn is_past(&self) -> bool {
        self.dispatch.is_past()
    }

    /// Whether the record's latest edits are durable, as
    /// [`ViewCx::save_status`](super::ViewCx::save_status): the same state
    /// the host updates after every sync, for a surface that draws its own
    /// status line. A surface never tracks durability itself.
    pub fn save_status(&self) -> ui_core::LocalState<ui_core::record::RecordSaveStatus> {
        self.saved.clone()
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

    /// What the launch that opened the app named — its arguments, the file
    /// a host handed over, a playback handoff — as
    /// [`ViewCx::launch`](super::ViewCx::launch).
    pub fn launch(&self) -> super::launch::LaunchInput<'_> {
        self.run.launch()
    }

    /// What the window's launch named (paths, URLs), as
    /// [`ViewCx::launch_arguments`](super::ViewCx::launch_arguments).
    pub fn launch_arguments(&self) -> &[String] {
        &self.run.launched.arguments
    }

    /// The value of the app's own option `name` the window's launch named,
    /// as [`ViewCx::launch_option`](super::ViewCx::launch_option).
    pub fn launch_option(&self, name: &str) -> Option<&str> {
        super::launch::option(&self.run.launched.options, name)
    }

    /// Whether the window's launch named the app's switch `name`, as
    /// [`ViewCx::launch_flag`](super::ViewCx::launch_flag).
    pub fn launch_flag(&self, name: &str) -> bool {
        super::launch::flag(&self.run.launched.options, name)
    }

    /// The service of type `T` the host that mounts the app in its own
    /// process lent it ([`HostContext::lend`](ui_core::application::HostContext::lend)),
    /// taken once: the shell's connection to the session's process
    /// supervisor for a terminal it hosts. `None` in the app's own process,
    /// and for a past version or a capture, which start nothing.
    pub fn take_lent<T: 'static>(&self) -> Option<T> {
        if !self.run.mode.is_live() {
            return None;
        }
        self.run.launched.lent.take::<T>()
    }

    /// Whether the window is private, as
    /// [`ViewCx::is_private`](super::ViewCx::is_private).
    pub fn is_private(&self) -> bool {
        self.run.private
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

    /// The device this window runs on, as [`ViewCx::device`](super::ViewCx::device).
    pub fn device(&self) -> Option<&str> {
        self.storage.device()
    }

    /// The person's own record of `P` ([`PersonRecord`](super::PersonRecord)),
    /// as [`ViewCx::person`](super::ViewCx::person) opens it.
    pub fn person<P: tilo_resource::Resource>(&self) -> Option<super::PersonRecord<P>> {
        self.storage.person::<P>()
    }

    /// The person's shared record `P`, as
    /// [`ViewCx::shared_person`](super::ViewCx::shared_person) opens it.
    pub fn shared_person<P: tilo_resource::SharedRecord>(&self) -> Option<super::PersonRecord<P>> {
        self.storage.shared_person::<P>()
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
    ) -> Projection
    where
        K: Eq + Hash + Clone + 'static,
        T: 'static,
    {
        self.effects.drain(self.alive.clone(), pending, run)
    }

    /// Carry out the entries of an outbox this device owns, as
    /// [`ViewCx::outbox`](super::ViewCx::outbox).
    pub fn outbox<E, O>(
        &self,
        outbox: tilo_resource::Path<R, tilo_resource::Outbox<E, O>>,
        owns: impl Fn(&R::State, &E) -> bool + 'static,
        run: impl FnMut(&tilo_resource::Id, E, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> Projection
    where
        R: Resource<State = R> + tilo_resource::Model + 'static,
        E: tilo_resource::Model + Clone + 'static,
        O: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
    {
        self.effects
            .outbox(self.alive.clone(), outbox, owns, run, false)
    }

    /// [`ViewCx::outbox_awaited`](super::ViewCx::outbox_awaited).
    pub fn outbox_awaited<E, O>(
        &self,
        outbox: tilo_resource::Path<R, tilo_resource::Outbox<E, O>>,
        owns: impl Fn(&R::State, &E) -> bool + 'static,
        run: impl FnMut(&tilo_resource::Id, E, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> Projection
    where
        R: Resource<State = R> + tilo_resource::Model + 'static,
        E: tilo_resource::Model + Clone + 'static,
        O: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
    {
        self.effects
            .outbox(self.alive.clone(), outbox, owns, run, true)
    }

    /// Caller-authorized document consequences, as [`ViewCx::drain_causal`](super::ViewCx::drain_causal).
    pub fn drain_causal<K, T>(
        &self,
        pending: impl Fn(&R::State) -> Vec<(K, T)> + 'static,
        run: impl FnMut(T, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) where
        K: Eq + Hash + Clone + 'static,
        T: 'static,
    {
        self.effects.drain_causal(self.alive.clone(), pending, run);
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
    /// The jobs the surface started on its own.
    jobs: OwnedJobs<R, S>,
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
            jobs: OwnedJobs::new(jobs),
            surface,
            search_queries: SearchQueries::default(),
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

    // The host is invisible to whoever reaches the surface by its type (a
    // test driving it, a host that embeds it): it answers as the surface.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(&self.surface)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut self.surface)
    }

    fn settle(&mut self, edits: &mut ui_core::widget::Edits<'_>, path: &ObjId) {
        self.surface.settle(edits, path);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
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
        if let Some(query) = self.search_queries.take()
            && self.effects.is_some()
        {
            self.surface.search_query(&query);
        }
        self.jobs.poll(&mut self.surface);
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
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if let Some(runner) = self.effects.as_mut()
            && let Some(started) =
                runner.start(op, &mut self.surface, Some(doc.doc()), S::effect)?
        {
            return Ok(Some(started));
        }
        // The surface answers its operations as its effect vocabulary; only
        // the toolkit's own widgets inside it (a button bound to a view)
        // answer theirs, never the surface by hand.
        let before = doc.pending_ops();
        let started = ui_core::host_operation::start_in_children(&mut self.surface, op, doc, path);
        refuse_raw_writes(doc, before, "start_operation");
        started
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.surface.update(state, path)
    }

    fn handle_event(
        &mut self,
        event: &ui_core::winit::event::WindowEvent,
        ctx: &mut ui_core::widget::EventCtx<'_>,
    ) {
        let before = ctx.state.pending_ops();
        self.surface.handle_event(event, ctx);
        refuse_raw_writes(ctx.state.doc(), before, "handle_event");
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui_core::a11y::Action,
        state: &mut ui_core::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let surface = &mut self.surface;
        let mut performed = false;
        only_through_actions(state, path, "perform_semantic_action", |state, path| {
            performed = surface.perform_semantic_action(target, action, state, path);
        });
        performed
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
        self.surface.children_mut().into_iter().find_map(|child| {
            child.poll_resource_operation(invocation, cancellation_requested, doc, path)
        })
    }
}

impl<R: Resource, S: ResourceSurface<R, E> + 'static, E: Effect> SurfacePublication<R>
    for SurfaceHost<R, S, E>
{
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

    fn bound_declarations(&self) -> Vec<ActionDecl<R>> {
        self.surface.bound_declarations()
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

    fn row_declarations(&self, row: &str) -> Vec<ActionDecl<R>> {
        self.surface.row_declarations(row)
    }

    fn search_entries(&self) -> Vec<SearchEntry> {
        self.surface.search_entries()
    }

    fn search_queries(&self) -> SearchQueries {
        self.search_queries.clone()
    }
}

// What the surface publishes as a widget passes through here, without
// document actions: those are the resource's, through its dispatcher.
impl<R: Resource, S: ResourceSurface<R, E> + 'static, E: Effect>
    ui_core::resources::PublishesResources for SurfaceHost<R, S, E>
{
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        ui_core::resources::resources(&self.surface, doc)
            .into_iter()
            .map(outside_document)
            .collect()
    }

    fn caller_view_resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        ui_core::resources::caller_view_resources(&self.surface, doc)
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
            &self.surface,
            doc,
            cursor,
            max_results,
        )?;
        page.resources = page.resources.into_iter().map(outside_document).collect();
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
        ui_core::resources::resource(&self.surface, doc, id).map(resolved_outside_document)
    }

    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        ui_core::resources::resources_for(&self.surface, participant, doc)
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
        ui_core::resources::resource_for(&self.surface, participant, doc, id)
            .map(resolved_outside_document)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ui_core::automerge::ROOT;
    use ui_core::automerge::transaction::Transactable;

    #[test]
    fn what_a_surface_applied_through_actions_stays() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "title", "applied").unwrap();
        // The executor commits every action it applies.
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
