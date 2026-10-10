//! Building views: widgets whose values come from the resource and whose
//! edits are actions.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::{Rc, Weak};

use tilo_actions::ActionBinding;
use tilo_resource::{ActionDecl, Binds, Collection, Effect, Id, Invoke, Resource};
use ui_core::LocalState;
use ui_core::record::RecordSaveStatus;
use ui_core::widget::{Button, Checkbox, Element, Text, TextInput};

use super::controls::{ActionControl, Actions};
use super::dispatch::Dispatcher;
use super::each::Each;
use super::effect::{EffectAnswer, EffectCx, EffectStatus, Effects, Projection};
use super::live::Live;
use super::scenario::{Run, RunMode};
use super::settings::{Setting, ViewStorage};
use super::stage::Stages;
use super::surface::{ResourceSurface, SurfaceCx, SurfaceHost};
use super::switch::Switch;

fn disabled(mut button: Button) -> Button {
    button.set_enabled(false);
    button
}

type Update<R> = Box<dyn FnMut(&<R as Resource>::State)>;
type Updater<R> = (Weak<()>, Update<R>);

/// Model-to-widget refreshers registered by bound controls. Each is tied to
/// the lifetime of the view scope that created it (a part a
/// [`ViewCx::switch`] built). The root view has one list; each row of an
/// [`Each`] has its own, which the list runs while the row is mounted and
/// catches up when it mounts again.
pub(crate) struct Updaters<R: Resource> {
    list: Rc<RefCell<Vec<Updater<R>>>>,
    /// Whether something besides the row's own widgets reads what these
    /// updaters feed — an edit the scope binds into the published tree, an
    /// effect's availability — so they must stay current while the row is
    /// not mounted.
    read_unmounted: Rc<std::cell::Cell<bool>>,
}

impl<R: Resource> Clone for Updaters<R> {
    fn clone(&self) -> Self {
        Self {
            list: Rc::clone(&self.list),
            read_unmounted: Rc::clone(&self.read_unmounted),
        }
    }
}

impl<R: Resource> Updaters<R> {
    pub(crate) fn new() -> Self {
        Self {
            list: Rc::default(),
            read_unmounted: Rc::default(),
        }
    }

    pub(crate) fn run(&self, state: &R::State) {
        // Take the list so an updater may register more (a rebuilt row)
        // without a reentrant borrow.
        let mut current = std::mem::take(&mut *self.list.borrow_mut());
        current.retain_mut(|(alive, update)| {
            if alive.strong_count() == 0 {
                return false;
            }
            update(state);
            true
        });
        let mut list = self.list.borrow_mut();
        current.append(&mut list);
        *list = current;
    }

    fn push(&self, alive: Weak<()>, update: Update<R>) {
        self.list.borrow_mut().push((alive, update));
    }

    /// Something outside the scope's widgets reads its local state.
    fn read_while_unmounted(&self) {
        self.read_unmounted.set(true);
    }

    /// Whether the updaters must run while their row is not mounted.
    pub(crate) fn read_unmounted(&self) -> bool {
        self.read_unmounted.get()
    }
}

type Bind<R> = Box<dyn Fn(&<R as Resource>::State) -> Vec<ActionDecl<R>>>;
type BindList<R> = Vec<(Weak<()>, Bind<R>)>;

/// The edits views bound to their local state
/// ([`ViewCx::bind_declarations`]), each tied to the view scope that bound
/// it.
pub(crate) struct Bindings<R: Resource>(Rc<RefCell<BindList<R>>>);

impl<R: Resource> Clone for Bindings<R> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<R: Resource> Bindings<R> {
    pub(crate) fn new() -> Self {
        Self(Rc::default())
    }

    /// Every live view's bound edits over `state`.
    pub(crate) fn declarations(&self, state: &R::State) -> Vec<ActionDecl<R>> {
        let mut bindings = self.0.borrow_mut();
        bindings.retain(|(alive, _)| alive.strong_count() > 0);
        bindings.iter().flat_map(|(_, bind)| bind(state)).collect()
    }
}

/// Projects the resource state onto the value a view scope reads: the whole
/// state at the root, one item inside an [`Each`] row.
pub(crate) type Focus<R, T> = Rc<dyn for<'a> Fn(&'a <R as Resource>::State) -> Option<&'a T>>;

/// Everything a view needs: reads of the resource state, bindings of controls
/// to actions, local state, and custom surfaces.
///
/// `T` is what this scope reads — the resource state at the root, one item in
/// a row built by [`ViewCx::each`].
pub struct ViewCx<R: Resource, T: 'static = <R as Resource>::State> {
    pub(crate) live: Live<R>,
    pub(crate) focus: Focus<R, T>,
    pub(crate) updaters: Updaters<R>,
    pub(crate) alive: Weak<()>,
    pub(crate) dispatch: Dispatcher<R>,
    pub(crate) action_bindings: Actions<R>,
    pub(crate) item: Option<Id>,
    pub(crate) controls: Rc<RefCell<HashMap<String, u32>>>,
    pub(crate) saved: LocalState<RecordSaveStatus>,
    pub(crate) stages: Stages,
    pub(crate) feeds: super::history::HistoryFeeds,
    pub(crate) run: Run,
    pub(crate) storage: Rc<ViewStorage>,
    pub(crate) bindings: Bindings<R>,
    pub(crate) effects: Effects<R>,
}

impl<R: Resource> ViewCx<R> {
    /// The scope of a freshly built root view: its own updaters, save status
    /// and staging, none of which another root shares.
    pub(crate) fn root(
        live: Live<R>,
        alive: &Rc<()>,
        dispatch: Dispatcher<R>,
        published: tilo_actions::Resource,
        run: Run,
        storage: Rc<ViewStorage>,
    ) -> Self {
        // The history verbs every app answers are bindable like its own.
        let mut published = published;
        for spec in super::history::specs() {
            if !published.actions.iter().any(|action| action.id == spec.id) {
                published = published.with_action(spec);
            }
        }
        Self {
            live,
            focus: Rc::new(|state| Some(state)),
            updaters: Updaters::new(),
            alive: Rc::downgrade(alive),
            dispatch,
            action_bindings: Actions::new(Rc::new(published)),
            item: None,
            controls: Rc::default(),
            saved: LocalState::new(RecordSaveStatus::Saved),
            stages: Stages::new(),
            feeds: super::history::HistoryFeeds::default(),
            run,
            storage,
            bindings: Bindings::new(),
            effects: Effects::inert(),
        }
    }
}

impl<R: Resource, T: 'static> ViewCx<R, T> {
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

    /// The names of the participants, for showing the ids actions record
    /// ([`People::name`](super::People::name)).
    pub fn people(&self) -> super::People {
        self.run.people.clone()
    }

    /// The live resource state, for reads a binding does not cover.
    pub fn live(&self) -> &Live<R> {
        &self.live
    }

    /// Where edits go. Custom widgets keep a clone.
    pub fn dispatcher(&self) -> Dispatcher<R> {
        self.dispatch.clone()
    }

    /// Observe what this participant may do, without storing their role in
    /// the shared resource. Use this with [`Self::switch`] for an owner-only
    /// pane, or with a bound widget for a permission-specific label. Changes
    /// follow the host even when the document and write access stay unchanged.
    /// This controls presentation; the action door remains the authority.
    pub fn permission_state(
        &self,
        requirement: tilo_actions::PermissionRequirement,
    ) -> LocalState<bool> {
        self.dispatch.permission_state(requirement)
    }

    /// The dispatcher of the resource nested in this one at `id`, whose
    /// edits are this resource's action `wrap(edit)`: a rich-text body, a
    /// draft, a SQL buffer. See [`Dispatcher::child`].
    pub fn child<C: Resource>(
        &self,
        id: &str,
        wrap: impl Fn(C::Action) -> R::Action + 'static,
    ) -> Dispatcher<C> {
        self.dispatch.child(id, wrap)
    }

    /// Who this view is for: the participant reading and editing through
    /// it, the same id an action they invoke reads as its author
    /// ([`ActionCx::author`](tilo_resource::ActionCx::author)). Compare the
    /// authors the resource records with it ("your message", "you reacted")
    /// instead of keeping the reader in the resource state, which is the
    /// same for everyone. `None` in a view of a past version.
    pub fn me(&self) -> Option<&str> {
        self.dispatch.me()
    }

    /// What the launch that opened this window named — the file a person
    /// opened the app with, its command line — for a view that shows what
    /// the launch's actions ([`ResourceApp::launch`](super::ResourceApp::launch))
    /// made: the note a launch imported, opened. Empty for a plain start and
    /// for a past version.
    pub fn launch(&self) -> super::launch::LaunchInput<'_> {
        self.run.launch()
    }

    /// Whether this participant may only view, when the view is built.
    /// Bound controls already follow permission changes on their own; to show
    /// or disable something else while the participant may only view, bind to
    /// [`Self::read_only_state`], which follows role changes while the app
    /// runs.
    pub fn read_only(&self) -> bool {
        self.dispatch.read_only()
    }

    /// Whether this view shows a past version in History mode, drawn from a
    /// frozen document. It must start no work there: a surface that fetches,
    /// watches or spawns a worker is built without it and presents what the
    /// document holds.
    pub fn is_past(&self) -> bool {
        self.dispatch.is_past()
    }

    /// Whether this participant may only view, as state that follows role
    /// changes while the app runs.
    pub fn read_only_state(&self) -> LocalState<bool> {
        self.dispatch.read_only_state()
    }

    /// Whether the record's latest edits are durable, for a status line.
    /// Saving is automatic; there is nothing to save by hand.
    pub fn save_status(&self) -> LocalState<RecordSaveStatus> {
        self.saved.clone()
    }

    /// The item this scope stands for, inside an [`Each`] row.
    pub fn item(&self) -> Option<&Id> {
        self.item.as_ref()
    }

    /// The resource's last `last` versions, oldest first, kept current as
    /// the document moves: an in-app history list of what was done and by
    /// whom, each version addressable by the `undo`, `restore` and
    /// `checkpoint` actions every app publishes.
    pub fn history(&self, last: usize) -> LocalState<Vec<super::history::HistoryVersion>> {
        self.feeds.follow(self.alive.clone(), last)
    }

    /// Read the scope's value now (at build time). For values that must
    /// follow the resource, use a binding such as [`Self::text`].
    pub fn read<V>(&self, read: impl FnOnce(Option<&T>) -> V) -> V {
        self.live.with(|state| read((self.focus)(state)))
    }

    /// What the view is built for: a window someone uses, a past version in
    /// History, or a screenshot. Start work that reaches outside the process
    /// (an engine, a socket, a probe) only when it [is live](RunMode::is_live).
    pub fn mode(&self) -> RunMode {
        self.run.mode
    }

    /// The capture's stand-in for a live source of type `V`
    /// ([`Fixture::provide`](super::Fixture::provide)); `None` outside a
    /// capture, or when the scenario provides none.
    pub fn fixture<V: 'static>(&self) -> Option<Rc<V>> {
        self.run.fixture::<V>()
    }

    /// The content store the host lent the app: opaque blobs by hash that
    /// outlive it. A capture's holds the blobs its scenario put there.
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        self.run.content()
    }

    /// What the window's launch named (paths, URLs), for a view that acts
    /// on them beyond the actions the launch took: the file a save writes
    /// back to. Empty for a plain start, a past version and a capture.
    pub fn launch_arguments(&self) -> &[String] {
        &self.run.launched.arguments
    }

    /// The value of the app's own option `name` (`--purpose`) the window's
    /// launch named; see [`ResourceApp::OPTIONS`](super::ResourceApp::OPTIONS).
    /// `None` for a past version.
    pub fn launch_option(&self, name: &str) -> Option<&str> {
        super::launch::option(&self.run.launched.options, name)
    }

    /// Whether the window's launch named the app's switch `name`; see
    /// [`ResourceApp::FLAGS`](super::ResourceApp::FLAGS).
    pub fn launch_flag(&self, name: &str) -> bool {
        super::launch::flag(&self.run.launched.options, name)
    }

    /// The service of type `T` the host that mounts the app in its own
    /// process lent it ([`HostContext::lend`](ui_core::application::HostContext::lend)),
    /// taken once: the shell's connection to the session's process
    /// supervisor for a terminal it hosts. `None` in the app's own process,
    /// and for a past version or a capture, which start nothing.
    pub fn take_lent<S: 'static>(&self) -> Option<S> {
        if !self.run.mode.is_live() {
            return None;
        }
        self.run.launched.lent.take::<S>()
    }

    /// Whether the window is private ([`ResourceApp::PRIVATE_WINDOWS`](super::ResourceApp::PRIVATE_WINDOWS)):
    /// its record, settings and files are thrown away with the process. A
    /// view keeps what it would otherwise remember on the device (visits,
    /// a session to restore) to itself.
    pub fn is_private(&self) -> bool {
        self.run.private
    }

    /// The app's typed settings of type `S`, kept current while this scope
    /// lives. `S` declares whether they follow the person or stay on the
    /// device ([`Scope`](ui_core::storage::Scope)).
    pub fn settings<S>(&self) -> Setting<S>
    where
        S: ui_core::storage::SettingsSchema + Clone + PartialEq + Send + Sync,
    {
        self.storage.follow(self.alive.clone())
    }

    /// The device this window runs on: stable across launches, distinct on
    /// each of the person's devices. `None` in a view of a past version. An
    /// action that records where something happened (a speed test run on
    /// this device) takes it as an argument, so every replica applies the
    /// same value.
    pub fn device(&self) -> Option<&str> {
        self.storage.device()
    }

    /// The person's own record of `P` ([`PersonRecord`](super::PersonRecord)):
    /// theirs alone, kept in their own room and synced to their other
    /// devices, never to a collaborator, whatever room this record is in.
    /// `None` where the host keeps no records of the person's (a view of a
    /// past version) or the record cannot be opened.
    pub fn person<P: Resource>(&self) -> Option<super::PersonRecord<P>> {
        self.storage.person::<P>()
    }

    /// The person's record `P` that one app keeps for all of theirs — their
    /// accounts, which Settings keeps: the same record on every device, held
    /// here too and converging with the keeper's over the person's own room.
    /// Changed only by its owner and editors
    /// ([`SharedRecord`](tilo_resource::SharedRecord),
    /// [`PersonRecord::editable`](super::PersonRecord::editable)); for its
    /// owner it is [`Self::person`].
    pub fn shared_person<P: tilo_resource::SharedRecord>(&self) -> Option<super::PersonRecord<P>> {
        self.storage.shared_person::<P>()
    }

    /// The app's files, secrets and settings documents, as its host gave
    /// them. A secret of one document item is
    /// `cx.storage().secrets().item(purpose, id)`.
    pub fn storage(&self) -> &ui_core::storage::AppStorage {
        self.storage.storage()
    }

    /// Typed process-local view state: never persisted, shared or undone.
    pub fn local<V: Send + Sync + 'static>(&self, initial: V) -> LocalState<V> {
        LocalState::new(initial)
    }

    /// Show the item a search hit opens: `open` runs with the hit's anchor
    /// (a [`SearchEntry`](tilo_resource::SearchEntry)'s `anchor`) when the
    /// person picks the hit, or when an agent invokes the resource's `open`
    /// action. Select the item, switch to its page, scroll it into view:
    /// set the [`LocalState`]s that show it, as the person's own input would.
    pub fn on_open(&self, open: impl FnMut(&str) + 'static) {
        self.stages.push_opener(self.alive.clone(), Box::new(open));
    }

    /// Open the platform search with `request`, narrowed to this app or one
    /// part of it. Usually a control does this ([`Self::search_button`]); a
    /// custom handler calls [`Dispatcher::open_search`] on its dispatcher.
    pub fn open_search(&self, request: &ui_core::search::SearchRequest) {
        self.dispatch.open_search(request);
    }

    /// The search launcher for this view's chrome: an icon button that opens
    /// the platform search with `request`. Apps never draw a search field.
    pub fn search_button(&self, request: ui_core::search::SearchRequest) -> Button {
        ui_core::search::search_button(request)
    }

    /// Stage view state for a screenshot scenario: `stage` runs with the
    /// scenario's name (`None` for a plain `--shots`) after the scenario's
    /// actions applied and were laid out, and before the frame is captured.
    /// Set the [`LocalState`]s a scenario shows — a selection, an open
    /// dialog — the way the user's input would. It never runs in a live
    /// window.
    pub fn on_stage(&self, stage: impl FnMut(Option<&str>) + 'static) {
        self.stages.push(self.alive.clone(), Box::new(stage));
    }

    fn reader<V: Default + 'static>(
        &self,
        read: impl Fn(&T) -> V + 'static,
    ) -> impl Fn(&R::State) -> V + 'static {
        let focus = Rc::clone(&self.focus);
        move |state| (focus)(state).map(&read).unwrap_or_default()
    }

    /// A stable, addressable control name: the action id, qualified by the
    /// row's item, numbered only when the same binding repeats.
    fn control_id(&self, action: &str) -> String {
        let base = match &self.item {
            Some(item) => format!("{action}-{item}"),
            None => action.to_owned(),
        };
        let mut seen = self.controls.borrow_mut();
        let count = seen.entry(base.clone()).or_insert(0);
        *count = count.saturating_add(1);
        if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        }
    }

    fn binding<I: 'static>(&self, binds: impl Binds<R, I>) -> Option<ActionBinding<I>> {
        self.actions().binding(binds)
    }

    /// Keep `sink` in step with a value derived from the resource.
    fn follow<V: PartialEq + Default + Clone + 'static>(
        &self,
        read: impl Fn(&T) -> V + 'static,
        mut sink: impl FnMut(V) + 'static,
    ) -> V {
        let read = self.reader(read);
        let initial = self.live.with(&read);
        let mut last = initial.clone();
        self.updaters.push(
            self.alive.clone(),
            Box::new(move |state| {
                let next = read(state);
                if next != last {
                    last.clone_from(&next);
                    sink(next);
                }
            }),
        );
        initial
    }

    /// Text that always shows `read` of the resource.
    pub fn text(&self, read: impl Fn(&T) -> String + 'static) -> Text {
        let live = self.live.clone();
        let read = self.reader(read);
        Text::dynamic(move || live.with(&read))
    }

    /// A text field showing `read` of the resource; each committed edit
    /// invokes `action` with the field's new text.
    pub fn input(
        &self,
        read: impl Fn(&T) -> String + 'static,
        action: impl Binds<R, String>,
    ) -> TextInput {
        let id = self.control_id(action.action_id());
        let state = LocalState::new(String::new());
        let sink = state.clone();
        let initial = self.follow(read, move |value| sink.set(value));
        state.set(initial);
        let input = TextInput::from_state(&state).with_id(&id);
        match self.binding(action) {
            Some(binding) => input.on_action(binding),
            None => input.disabled(true),
        }
    }

    /// Route a serialized screen's text inputs over `path` through the
    /// resource ([`UiValues::route`](ui_core::UiValues::route)): they show
    /// `read` of it and each edit invokes `action` with the input's new text.
    /// A view that may not invoke `action` leaves the path unrouted.
    pub fn route_graph_text(
        &self,
        values: &ui_core::UiValues,
        path: &str,
        read: impl Fn(&T) -> String + 'static,
        action: impl Binds<R, String>,
    ) {
        let Some(binding) = self.binding(action) else {
            return;
        };
        let shown = LocalState::new(String::new());
        let sink = shown.clone();
        let initial = self.follow(read, move |value| sink.set(value));
        shown.set(initial);
        values.route(path, shown, binding);
    }

    /// Route a serialized screen's checked controls through the resource:
    /// they show `read` and each edit invokes its typed `action`.
    pub fn route_graph_bool(
        &self,
        values: &ui_core::UiValues,
        path: &str,
        read: impl Fn(&T) -> bool + 'static,
        action: impl Binds<R, bool>,
    ) {
        let Some(binding) = self.binding(action) else {
            return;
        };
        let shown = LocalState::new(false);
        let sink = shown.clone();
        let initial = self.follow(read, move |value| sink.set(value));
        shown.set(initial);
        values.route_bool(path, shown, binding);
    }

    /// A checkbox showing `read` of the resource; toggling invokes `action`
    /// with the new value.
    pub fn checkbox(
        &self,
        label: impl Into<String>,
        read: impl Fn(&T) -> bool + 'static,
        action: impl Binds<R, bool>,
    ) -> Checkbox {
        let id = self.control_id(action.action_id());
        let state = LocalState::new(false);
        let sink = state.clone();
        let initial = self.follow(read, move |value| sink.set(value));
        state.set(initial);
        let checkbox = Checkbox::from_state(&state).with_id(&id).with_label(label);
        match self.binding(action) {
            Some(binding) => checkbox.on_action(binding),
            None => checkbox.with_enabled(false),
        }
    }

    /// A button that invokes one fully specified action.
    pub fn button(&self, label: &str, invoke: Invoke<R>) -> Button {
        let id = self.control_id(invoke.id);
        let button = Button::new(&id, label);
        match self.actions().invocation(invoke) {
            Some(binding) => button.on_action(binding),
            None => disabled(button),
        }
    }

    /// One row per item of a collection, kept in collection order. A row is
    /// built once per item and retained while the item exists, so focus and
    /// caret inside it survive other items changing. Inside a scroll view
    /// only the rows near what it shows are mounted, and only theirs follow
    /// each change; a row out of view catches up as it scrolls back in, so
    /// a change costs the rows shown however long the list. A row's context menu
    /// (right-click, the Menu key or Shift+F10) offers the actions the
    /// resource publishes on its item, with the item already bound.
    pub fn each<I: 'static>(
        &self,
        items: fn(&T) -> &Collection<I>,
        row: impl Fn(&mut ViewCx<R, I>) -> Element + 'static,
    ) -> Each<R> {
        let focus = Rc::clone(&self.focus);
        let ids: super::each::Ids<R> = Box::new(move |state, visit| match (focus)(state) {
            Some(value) => visit(&mut items(value).ids()),
            None => visit(&mut std::iter::empty()),
        });
        let parent_focus = Rc::clone(&self.focus);
        let live = self.live.clone();
        let dispatch = self.dispatch.clone();
        let action_bindings = self.action_bindings.clone();
        let controls = Rc::clone(&self.controls);
        let saved = self.saved.clone();
        let stages = self.stages.clone();
        let feeds = self.feeds.clone();
        let run = self.run.clone();
        let storage = Rc::clone(&self.storage);
        let bindings = self.bindings.clone();
        let effects = self.effects.clone();
        Each::new(
            self.live.clone(),
            self.dispatch.clone(),
            ids,
            Box::new(move |id: &Id, alive: &Rc<()>, updaters: &Updaters<R>| {
                let parent = Rc::clone(&parent_focus);
                let key = id.clone();
                // Where the item stood last time: a row reads its item
                // without scanning the collection.
                let near = std::cell::Cell::new(0_usize);
                let focus: Focus<R, I> = Rc::new(move |state| {
                    let (at, item) = items((parent)(state)?).get_near(&key, near.get())?;
                    near.set(at);
                    Some(item)
                });
                let mut cx = ViewCx {
                    live: live.clone(),
                    focus,
                    updaters: updaters.clone(),
                    alive: Rc::downgrade(alive),
                    dispatch: dispatch.clone(),
                    action_bindings: action_bindings.clone(),
                    item: Some(id.clone()),
                    controls: Rc::clone(&controls),
                    saved: saved.clone(),
                    stages: stages.clone(),
                    feeds: feeds.clone(),
                    run: run.clone(),
                    storage: Rc::clone(&storage),
                    bindings: bindings.clone(),
                    effects: effects.clone(),
                };
                row(&mut cx)
            }),
        )
    }

    /// Publish edits bound to this view's local state — the folder shown,
    /// the row selected — on the resource beside its declared actions, as
    /// typed builders of its action: `bind` runs whenever the published
    /// tree is read, with the current resource state, and reads the local
    /// state it captured. Invoking one dispatches what it builds through the
    /// same permission check, confirmation and undo as any other action. The
    /// binding lasts as long as this view scope (a row's ends with the row);
    /// an id the resource already publishes keeps the resource's action
    /// unless the bound one refines it ([`tilo_resource::bind_declaration`]).
    pub fn bind_declarations(&self, bind: impl Fn(&R::State) -> Vec<ActionDecl<R>> + 'static) {
        // The published tree reads what `bind` captured, mounted row or not.
        self.updaters.read_while_unmounted();
        self.bindings
            .0
            .borrow_mut()
            .push((self.alive.clone(), Box::new(bind)));
    }

    /// The resource's published actions, for binding controls built outside
    /// the view function: the rows a table, list or tree creates on demand.
    pub fn actions(&self) -> Actions<R> {
        self.action_bindings.clone()
    }

    /// A value that follows `read` of the resource, for any toolkit control
    /// built from a [`LocalState`] (`Slider::from_state`, `Dropdown::from_state`).
    pub fn value<V>(&self, read: impl Fn(&T) -> V + 'static) -> LocalState<V>
    where
        V: PartialEq + Default + Clone + 'static,
    {
        let state = LocalState::new(V::default());
        let sink = state.clone();
        let initial = self.follow(read, move |value| sink.set(value));
        state.set(initial);
        state
    }

    /// Any action-bound toolkit control, showing `read` of the resource and
    /// invoking `action` with each committed value:
    /// `cx.control(Slider::from_state, |p| p.volume, Profile::SET_VOLUME)`.
    /// The control is disabled when the action is not published.
    pub fn control<V, C>(
        &self,
        make: impl FnOnce(&LocalState<V>) -> C,
        read: impl Fn(&T) -> V + 'static,
        action: impl Binds<R, V>,
    ) -> C
    where
        V: PartialEq + Default + Clone + 'static,
        C: ActionControl<V>,
    {
        let control = make(&self.value(read));
        self.actions().bind(control, action)
    }

    /// A part of the view chosen by view-local state — the detail pane of the
    /// selected item, the page of a wizard. The part is built again, with its
    /// own bindings, whenever `key` changes; the previous one is dropped.
    pub fn switch<K>(
        &self,
        key: &LocalState<K>,
        part: impl Fn(&mut ViewCx<R, T>, &K) -> Element + 'static,
    ) -> Element
    where
        K: Clone + PartialEq + 'static,
    {
        let live = self.live.clone();
        let focus = Rc::clone(&self.focus);
        let updaters = self.updaters.clone();
        let dispatch = self.dispatch.clone();
        let action_bindings = self.action_bindings.clone();
        let item = self.item.clone();
        let saved = self.saved.clone();
        let stages = self.stages.clone();
        let feeds = self.feeds.clone();
        let run = self.run.clone();
        let storage = Rc::clone(&self.storage);
        let bindings = self.bindings.clone();
        let effects = self.effects.clone();
        Box::new(Switch::new(
            key.clone(),
            Box::new(move |value: &K, alive: &Rc<()>| {
                let mut cx = ViewCx {
                    live: live.clone(),
                    focus: Rc::clone(&focus),
                    updaters: updaters.clone(),
                    alive: Rc::downgrade(alive),
                    dispatch: dispatch.clone(),
                    action_bindings: action_bindings.clone(),
                    item: item.clone(),
                    // Each build of the part names its controls afresh.
                    controls: Rc::default(),
                    saved: saved.clone(),
                    stages: stages.clone(),
                    feeds: feeds.clone(),
                    run: run.clone(),
                    storage: Rc::clone(&storage),
                    bindings: bindings.clone(),
                    effects: effects.clone(),
                };
                part(&mut cx, value)
            }),
        ))
    }

    /// Host a custom widget or renderer that takes part through the
    /// [`ResourceSurface`] contract, answering its effect vocabulary `E`
    /// (none unless it implements `ResourceSurface<R, E>`).
    pub fn surface<S, E>(&self, mut surface: S) -> Element
    where
        S: ResourceSurface<R, E> + 'static,
        E: Effect,
    {
        let jobs = self.effects.surface_jobs();
        surface.attach(SurfaceCx {
            jobs: jobs.clone(),
            dispatch: self.dispatch.clone(),
            actions: self.actions(),
            live: self.live.clone(),
            run: self.run.clone(),
            storage: Rc::clone(&self.storage),
            effects: self.effects.clone(),
            saved: self.saved.clone(),
            alive: self.alive.clone(),
        });
        surface.present(&self.live.get());
        let host = SurfaceHost::new(
            self.live.clone(),
            self.dispatch.clone(),
            &self.effects,
            jobs,
            self.alive.clone(),
            surface,
        );
        let pending = host.staging();
        self.stages.push(
            self.alive.clone(),
            Box::new(move |scenario| {
                *pending.borrow_mut() = Some(scenario.map(str::to_owned));
            }),
        );
        Box::new(host)
    }

    /// Answer the effect vocabulary `E` — work with no document delta that a
    /// button, the palette, an agent or a script invokes — with `handle`,
    /// and publish `E` on the resource while this part of the view lives.
    ///
    /// `handle` receives each invocation as a typed value. It finishes at
    /// once ([`Effected::done`](super::Effected::done)) or starts a job
    /// through the [`EffectCx`] (for an effect declared `job`); a job's
    /// completion runs on the UI thread and changes the document through the
    /// context's dispatcher. Handlers never run in History mode. Say which
    /// effects are unavailable or relabelled right now with
    /// [`EffectsHandle::status`].
    pub fn effects<E: Effect>(
        &self,
        handle: impl FnMut(E, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> EffectsHandle {
        let status: StatusSource = Rc::default();
        // The handler and the status the resource publishes read the
        // scope's local state, mounted row or not.
        self.updaters.read_while_unmounted();
        // Captures and History still describe the controls they draw. Only
        // execution needs a live environment; publication is inert metadata.
        let source = Rc::clone(&status);
        self.effects.publish::<E>(self.alive.clone(), move || {
            let now = source
                .borrow()
                .as_ref()
                .map(|status| status())
                .unwrap_or_default();
            super::effect::published_specs::<E>(&now)
        });
        if self.effects.env().is_some() {
            self.effects.handle(self.alive.clone(), handle);
        }
        EffectsHandle { status }
    }

    /// Jobs the view starts on its own, with nobody invoking an effect: a
    /// fetch when what it shows changes. See [`Jobs`](super::Jobs).
    pub fn jobs(&self) -> super::Jobs<R> {
        self.effects.view_jobs()
    }

    /// Drain a projection: what the resource's actions queued for the world
    /// outside the document — an outbox entry, a filesystem operation, the
    /// device configuration the document describes.
    ///
    /// `pending` lists the entries waiting in a state, each under a stable
    /// key; `run` carries one out, at once or as a job through its
    /// [`EffectCx`], and records the outcome with an action (acknowledging
    /// the entry, so it is no longer pending). The framework runs it in the
    /// live app after every change, on every participant's device — never
    /// in History mode — and runs an entry again only when the document
    /// changed and still lists it, so `run` should be safe to repeat. A
    /// drain that compares the document against something outside it (what
    /// a service reports back) looks again when the [`Projection`] it
    /// answers is told the outside changed.
    pub fn drain<K, I>(
        &self,
        pending: impl Fn(&R::State) -> Vec<(K, I)> + 'static,
        run: impl FnMut(I, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> Projection
    where
        K: Eq + Hash + Clone + 'static,
        I: 'static,
    {
        // A drain runs after every change, mounted row or not.
        self.updaters.read_while_unmounted();
        self.effects.drain(self.alive.clone(), pending, run)
    }

    /// Drain only consequences newly queued by an actual local Document
    /// invocation. Delayed work keeps that invocation's checked custody, and
    /// the caller receives success only after every consequence settles.
    /// Remote/replayed/previously pending entries acquire no caller authority.
    pub fn drain_causal<K, I>(
        &self,
        pending: impl Fn(&R::State) -> Vec<(K, I)> + 'static,
        run: impl FnMut(I, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) where
        K: Eq + Hash + Clone + 'static,
        I: 'static,
    {
        self.updaters.read_while_unmounted();
        self.effects.drain_causal(self.alive.clone(), pending, run);
    }

    /// A button that invokes one effect, through the same door (and the
    /// same confirmation, for a destructive one) as the palette.
    pub fn effect_button<E: Effect>(&self, label: &str, effect: &E) -> Button {
        let id = self.control_id(effect.id());
        self.actions()
            .bind_effect_invocation(Button::new(&id, label), effect)
    }
}

type StatusSource = Rc<RefCell<Option<Box<dyn Fn() -> Vec<EffectStatus>>>>>;

/// A registered effect handler ([`ViewCx::effects`]).
pub struct EffectsHandle {
    status: StatusSource,
}

impl EffectsHandle {
    /// Publish the effects as `status` says they are whenever the resource
    /// is published: unavailable while offline, relabelled while recording.
    /// Read the view's [`LocalState`]s in it.
    pub fn status(self, status: impl Fn() -> Vec<EffectStatus> + 'static) {
        *self.status.borrow_mut() = Some(Box::new(status));
    }
}
