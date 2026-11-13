//! An app instance: one window of a tilolang app as its app host runs it, without the toolkit.
//!
//! The instance holds what the window shows and does, as data: the host over the workspace record
//! (or its indexes, before the record), the window's two views (the root, and the view of the
//! resource `open(r)` or a row went to), the rows of their virtual lists, what bound controls
//! show, and the commits waiting for the door. It sends its window component patches and hears
//! what the person did, over the patch protocol (`tilo_render_protocol::patch`), through any
//! transport: a queue beside a window in one process, the UI host's socket, or `postMessage`
//! from a worker. What builds, lays out, draws and takes input is the window's.
//!
//! Each frame the instance
//!
//! 1. brings the runtime's mirror of the workspace record up to date with the record,
//! 2. takes what the person did and runs it in the runtime, which plans the change,
//! 3. re-runs a view if what it read changed, and sends the window the patches that bring its
//!    copy of the view's tree up to date, and
//! 4. queues what the runtime planned for the door, as an invocation of the host's `commit`
//!    action, so the change is attributed, undoable and replayed by every peer.
//!
//! What holds for every window is the platform's, and so the instance's: the lenses on each
//! resource are what presence reports; a destructive action a person runs offers Undo; a
//! primary action a person runs shows what it made.

mod convert;
pub mod form;
mod person_state;
#[cfg(feature = "record")]
pub mod record;
pub mod rows;
mod shown;
mod views;

pub use convert::{
    PREVIEW_CHARS, inline_title, inline_view, link_of, preview, shown_values, window_tree,
};
pub(crate) use convert::{bound_values, convert_with, referenced_resource, rows_message};
pub mod wire;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value as Json;
use tilo_actions::{ActionAddress, ControlInvocation, InvocationId};
use tilo_render_protocol::patch::{self as ui, Lane, ToApp, ToUi, Transport, Words};

use crate::bundle::Bundle;
use crate::host::{Host, Opening, Package, RootView};
use crate::meta::MetaExt;
use crate::publish::{COMMIT, HOST_RESOURCE};
use crate::runtime::meta::{Meta, TypeMeta};
use crate::runtime::{
    ActionRef, BindTarget, Binding, EditOutcome, InvokeOutcome, NameHost, OutsideSink, Patch,
    Principal, RefValue, Resolution, ResourceRef, Suggestion, UiNode, Value, ViewInstance,
};
use crate::upgrade::Upgrade;
use rows::ListRows;
use wire::Convert;

/// What an instance runs as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A window someone uses.
    Live,
    /// A past version, previewed read-only: nothing it does is committed.
    Past,
    /// A capture of a scenario.
    Capture,
}

/// What the person asked of their history at the window: the record's undo or redo
/// (`ToApp::Undo`, `ToApp::Redo`), or an undo a view applied (`undo(v)`, `undo(run)`), as the
/// one who runs the instance carries it out on the record through its history verb `undo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryAsk {
    /// Take back their last step.
    Undo,
    /// Bring back what their last undo took back.
    Redo,
    /// Take back the step the change (a change id of the record) is part of.
    UndoStep(String),
    /// Take back every step of the run (its group).
    UndoRun(String),
}

/// A later build of the package, in a development session.
#[derive(Clone)]
pub struct Update {
    /// Counts the builds taken since launch, from 1.
    pub generation: u64,
    pub bundle: Arc<Bundle>,
}

/// What the instance publishes on the door while it lives: the workspace's records with their
/// actions, kept up to date change by change.
pub trait Publication {
    /// Bring what is published up to date with the record.
    fn update(&self);
    /// Whether records are still being read off the frame.
    fn pending(&self) -> bool;
    /// Bring what is published up to date, waiting for the records being read off the frame:
    /// for an embedder with no frame to take them in on, which reads what is published once it
    /// covers the record. Whether what is published changed.
    fn settle(&self) -> bool {
        self.update();
        false
    }
    /// How many records the updates so far have looked at, one per record per turn: what
    /// publishing a change costs, whatever the workspace holds.
    fn visited(&self) -> u64 {
        0
    }
    /// How many times the updates so far have read every record in full
    /// ([`Publications::sweeps`](crate::publish::Publications::sweeps)).
    fn sweeps(&self) -> u64 {
        0
    }
}

mod platform;

pub use platform::{Platform, PlatformBlobs, PlatformWeb, PlatformWords};

/// How an instance starts.
pub struct Config {
    /// The package the window runs.
    pub app: String,
    /// The participant at the window.
    pub me: String,
    pub mode: Mode,
    /// The device that carries out the outside effects the workspace queues.
    pub device: Option<String>,
    /// Where the indexes of the workspace are kept; `None` keeps them in memory.
    pub index_dir: Option<std::path::PathBuf>,
    /// Reads the workspace record when the instance opens over the indexes before it: a write, or a
    /// field no index holds, waits for it.
    pub record_loader: Option<Box<dyn Fn() -> Option<tilo_resource::Doc>>>,
    /// Where outside effects go; the development log when `None`.
    pub sink: Option<Box<dyn OutsideSink>>,
    /// The root and the view the window opens over, each by name. A package with more than one
    /// root (the shell's `Desk` and its `Arrangement`) says which layer this window is;
    /// [`RootView::default`] takes the package's first root and its default view, as the
    /// launcher opens an app.
    pub root: RootView,
}

/// Commits waiting to be handed to the door, each tagged with the invocation of the door that
/// made it, when one did: the door applies it for the invoker it admitted.
pub type Outgoing = Rc<RefCell<VecDeque<(ControlInvocation, Option<InvocationId>)>>>;

/// Hand everything the runtime committed under a ticket of `first` or later to the door.
/// Returns the tickets handed over.
pub fn queue_commits(
    host: &Host,
    outgoing: &Outgoing,
    first: u64,
    origin: Option<InvocationId>,
) -> Vec<u64> {
    let mut tickets = Vec::new();
    for committed in host.take_committed_from(first) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("ticket".to_owned(), Json::from(committed.ticket));
        if let Some(invocation) = ControlInvocation::from_parts(HOST_RESOURCE, COMMIT, arguments) {
            outgoing.borrow_mut().push_back((invocation, origin));
            tickets.push(committed.ticket);
        }
    }
    tickets
}

/// The names of the workspace, as the runtime asks for them.
#[derive(Clone)]
struct SharedNames(Rc<dyn NameHost>);

impl NameHost for SharedNames {
    fn name(&self, who: &Principal) -> String {
        self.0.name(who)
    }
}

/// How the platform reads a day or a time of day in a sentence, as the runtime asks for it.
struct PlatformShown(Rc<dyn Platform>);

impl crate::runtime::ShownHost for PlatformShown {
    fn shown(&self, kind: &str, value: &str) -> String {
        self.0.shown(kind, value)
    }

    fn clock(&self, hour: u32, minute: u32) -> String {
        self.0.clock(hour, minute)
    }
}

/// Where an edit the window made applies: the lane's own view showing the item, the lane's view
/// showing another item of the same list, or a view of the item opened for the edit.
enum Place {
    /// The lane's own view, showing the item.
    Here(Rc<RefCell<ViewInstance>>),
    /// The lane's own view, drawing another item of its page: `before` is the item it shows.
    Beside {
        view: Rc<RefCell<ViewInstance>>,
        before: ResourceRef,
    },
    /// A view of the item, opened for the edit.
    Fresh(Rc<RefCell<ViewInstance>>),
}

impl Place {
    /// Whether the lane shows the item the edit was made in.
    fn shown(&self) -> bool {
        matches!(self, Self::Here(_) | Self::Beside { .. })
    }

    fn view(&self) -> &Rc<RefCell<ViewInstance>> {
        match self {
            Self::Here(view) | Self::Beside { view, .. } | Self::Fresh(view) => view,
        }
    }

    fn into_view(self) -> Rc<RefCell<ViewInstance>> {
        match self {
            Self::Here(view) | Self::Beside { view, .. } | Self::Fresh(view) => view,
        }
    }
}

/// One of the window's two views: a resource shown through a view.
struct Shown {
    target: ResourceRef,
    /// The item of the window's own view this lane shows in place (a tree beside what it
    /// shows), when it shows one there: edits to its page apply to the lane's own view.
    in_place: Option<ResourceRef>,
    /// The view `open(r, view: v)` named, when it named one; the default view of the type
    /// otherwise.
    with: Option<String>,
    view: Rc<RefCell<ViewInstance>>,
    /// The tree the window holds, as last sent: patches are against it.
    sent: Option<ui::Node>,
    /// The rows of its virtual lists, by the list's key.
    lists: BTreeMap<String, ListRows>,
    /// What each bound control was last sent to show, by key.
    bound: HashMap<String, ui::Value>,
    /// The last splice of each bound control taken, by key.
    acked: HashMap<String, u64>,
}

impl Shown {
    fn new(target: ResourceRef, mut view: ViewInstance) -> Self {
        show_content_rows(&mut view);
        Self {
            target,
            in_place: None,
            with: None,
            view: Rc::new(RefCell::new(view)),
            sent: None,
            lists: BTreeMap::new(),
            bound: HashMap::new(),
            acked: HashMap::new(),
        }
    }
}

/// What a person's action, run from a control, waits for the door to commit: the notice of a
/// destructive one, which offers Undo once it is committed.
#[derive(Default)]
struct Pending {
    undo: HashMap<u64, Words>,
}

/// See the module docs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent frame flags of one window: started, staged, moved, loading, follows"
)]
pub struct Instance {
    app: String,
    /// The root and the view the window opens over, by name.
    root_view: RootView,
    me: String,
    mode: Mode,
    device: Option<String>,
    index_dir: Option<std::path::PathBuf>,
    record_loader: Option<Box<dyn Fn() -> Option<tilo_resource::Doc>>>,
    /// Where the host reads the person's standing in the workspace, once its embedder has one.
    standing: Rc<RefCell<Option<Rc<crate::authority::StandingSource>>>>,
    /// Whether the instance shows the workspace before its record, over its indexes.
    opening: bool,
    platform: Rc<dyn Platform>,
    names: Rc<dyn NameHost>,
    /// What the window said it calls people, which `names` reads first.
    told: Rc<crate::names::Told>,
    host: Rc<RefCell<Option<Host>>>,
    /// The chunks of the record the frame before the host opened handed over: the store reads and
    /// writes the resources the record's manifest places in them, and an index is built over them
    /// all rather than over the record's own chunk alone ([`Host::open`]).
    record_chunks: Option<crate::chunk::Chunks>,
    publication: Option<Box<dyn Publication>>,
    root: Option<Shown>,
    detail: Option<Shown>,
    /// The space the window last said each lane's view has (`ToApp::Space`): a view opened in
    /// the lane later starts from it.
    root_space: Option<crate::runtime::space::Space>,
    detail_space: Option<crate::runtime::space::Space>,
    transport: Box<dyn Transport>,
    /// What waits to be sent this frame.
    out: Vec<ToUi>,
    /// The lenses on each resource of the package, as presence last reported them.
    lenses: HashMap<String, Vec<(Principal, bool, bool)>>,
    lenses_changed: bool,
    /// The person and the workspace's policy the runtime's members were last read from.
    members_from: Option<(
        String,
        tilo_action_policy::WorkspacePolicy,
        crate::members::People,
    )>,
    /// How the policy's people are the runtime's principals, as presence pairs them.
    people: crate::members::People,
    names_changed: bool,
    /// The person's formats moved: what a view writes of a time or a day (`.clock()`, a date in a
    /// sentence) is written again, and what it shows is shown in the new style.
    formats_changed: bool,
    outgoing: Outgoing,
    in_flight: VecDeque<u64>,
    door_work: Rc<Cell<bool>>,
    open_requests: Rc<RefCell<Vec<crate::effects::OpenEffect>>>,
    /// Whether the view of a shell places what it opens on the canvas, rather than opening a
    /// detail lane of the shell's own window ([`Instance::place_opens`]).
    place_opens: bool,
    /// The resources a view of a shell opened since the last turn: the places the session
    /// arranges on the canvas.
    opens_to_place: Vec<(ResourceRef, Option<String>, bool)>,
    /// What a person's press of a primary action made, to open as they asked for it.
    pressed_opens: Vec<(u64, String)>,
    ask: Option<Suggestion>,
    settle_due: bool,
    sink: Option<Box<dyn OutsideSink>>,
    /// The outside services the package's types with an origin are kept in step with.
    origins: crate::origin::Origins,
    pending: Pending,
    /// The undos and redos the person asked for, in order, not yet taken.
    history: Vec<HistoryAsk>,
    /// The window's notice offers to take back the marks the person just moved seen, not the
    /// record's last change: it is about what the window said last.
    seen_offer: bool,
    /// The actions the person chose at the window (a press, the palette), with the press each
    /// is, in order, until the host runs them as the person's own steps.
    chosen: VecDeque<(u64, ActionAddress, Option<String>)>,
    /// The scenario a capture stages, until the host runs it.
    staged: Option<String>,
    /// Why the scenario a capture staged did not run whole, until the capture takes it: a
    /// capture of a half-run scenario is no picture of it.
    stage_failed: Option<String>,
    started: bool,
    /// The package has no root of its own to open: everything it shows is opened by its
    /// embedder ([`Instance::without_root`]).
    rootless: bool,
    loading: bool,
    moved: bool,
    /// Whether the package's migrations ran over the workspace record since the window opened.
    migrated: bool,
    /// Whether this replica is the workspace's origin, the one place the package's migrations run
    /// ([`Self::set_origin`]).
    origin: bool,
    /// The later build this instance runs, 0 for the installed one.
    generation: u64,
    follows: bool,
    /// A later build this instance took, until its embedder asks.
    taken_build: Option<u64>,
    /// The window's keyboard focus: the bound control it is in.
    focus: Option<(Lane, String)>,
    composing: bool,
    /// How many presses the window sent (`ToApp::Fire`, `ToApp::Invoke`) the instance: the
    /// number each press goes by, in the order they came.
    fired: u64,
    /// The number of the last press of a view's own control the instance made: its frame runs
    /// it, where an invocation is run by the host after the presses it came with.
    last_fire: u64,
    /// The press the host took from [`Self::take_chosen`] and runs: the item an action it runs
    /// shows is what the person typed after that press goes into.
    taken_press: u64,
    /// How many presses the instance ran in order: an invocation is run by the host after the
    /// presses it came with, so a later press can run before it.
    ran: u64,
    /// The presses that ran while one before them had not, kept until it does.
    ran_ahead: BTreeSet<u64>,
    /// How many of them it told the window it ran (`ToUi::Took`).
    fired_told: u64,
    /// How many times the window was sent what changed of the tree it shows, one per frame
    /// whose change reached it (a tree's patches, or the rows of its lists): the first tree
    /// after opening, and one per change.
    patches_sent: u64,
    /// The form open in the window: an action waiting for its arguments.
    form: Option<form::Pending>,
    /// How many forms the window was sent: the next one's id.
    forms: u64,
}

impl Instance {
    /// The instance of `config`, sending its window what it shows over `transport`.
    #[must_use]
    pub fn new(
        config: Config,
        platform: Rc<dyn Platform>,
        names: Rc<dyn NameHost>,
        transport: Box<dyn Transport>,
    ) -> Self {
        let people = crate::members::People::new(&config.me, []);
        let told = Rc::new(crate::names::Told::new(&config.me, names));
        Self {
            app: config.app,
            root_view: config.root,
            me: config.me,
            mode: config.mode,
            device: config.device,
            index_dir: config.index_dir,
            opening: config.record_loader.is_some(),
            record_loader: config.record_loader,
            standing: Rc::default(),
            platform,
            names: Rc::clone(&told) as Rc<dyn NameHost>,
            told,
            host: Rc::default(),
            record_chunks: None,
            publication: None,
            root: None,
            detail: None,
            root_space: None,
            detail_space: None,
            transport,
            out: Vec::new(),
            lenses: HashMap::new(),
            lenses_changed: false,
            members_from: None,
            people,
            names_changed: false,
            formats_changed: false,
            outgoing: Rc::default(),
            in_flight: VecDeque::new(),
            door_work: Rc::default(),
            open_requests: Rc::default(),
            place_opens: false,
            opens_to_place: Vec::new(),
            pressed_opens: Vec::new(),
            ask: None,
            settle_due: false,
            sink: Some(
                config
                    .sink
                    .unwrap_or_else(|| Box::new(crate::sink::LogSink::dev())),
            ),
            origins: Rc::default(),
            pending: Pending::default(),
            history: Vec::new(),
            seen_offer: false,
            chosen: VecDeque::new(),
            staged: None,
            stage_failed: None,
            started: false,
            rootless: false,
            loading: false,
            moved: false,
            migrated: false,
            origin: false,
            generation: 0,
            follows: false,
            taken_build: None,
            focus: None,
            composing: false,
            fired: 0,
            last_fire: 0,
            taken_press: 0,
            ran: 0,
            ran_ahead: BTreeSet::new(),
            fired_told: 0,
            patches_sent: 0,
            form: None,
            forms: 0,
        }
    }

    /// This instance on the device `device`: the one that carries out the outside effects its
    /// workspace queues. Before the host starts.
    pub fn set_device(&mut self, device: Option<String>) {
        self.device = device;
    }

    /// Keep the workspace's indexes in `dir` (`None` keeps them in memory). Before the host starts.
    pub fn set_index_dir(&mut self, dir: Option<std::path::PathBuf>) {
        self.index_dir = dir;
    }

    /// Run a package that has no root view of its own: what it shows is opened in a lane by its
    /// embedder ([`Self::open_with`]), as a view of another app's state is. Before the host
    /// starts.
    pub fn without_root(&mut self) {
        self.rootless = true;
    }

    /// Open over the workspace's indexes before its record, which `loader` reads: a write, or a
    /// field no index holds, waits for it. Before the host starts.
    pub fn set_record_loader(&mut self, loader: Box<dyn Fn() -> Option<tilo_resource::Doc>>) {
        self.opening = true;
        self.record_loader = Some(loader);
    }

    /// Read the person's standing in the workspace from `source`: what the runtime invokes is decided
    /// by the workspace's policy with it. May come after the host started (a window shown before its
    /// host gave it one).
    pub fn set_standing(&self, source: Rc<crate::authority::StandingSource>) {
        *self.standing.borrow_mut() = Some(source);
    }

    /// Send the outside effects the instance settles to `sink`. Before the host starts.
    pub fn set_sink(&mut self, sink: Box<dyn OutsideSink>) {
        self.sink = Some(sink);
    }

    /// Keep the package's types with an origin in step with `origin`'s service, and hand it the
    /// outside effects of its capabilities.
    pub fn add_origin(&mut self, origin: crate::origin::SharedOrigin) {
        self.origins.borrow_mut().push(origin);
    }

    /// Poll each origin: what the services said is written and handed to the door.
    fn poll_origins(&mut self) -> bool {
        if self.opening
            || self.mode != Mode::Live
            || !self
                .origins
                .borrow()
                .iter()
                .any(|origin| origin.borrow().due())
        {
            return false;
        }
        let Some(host) = self.host() else {
            return false;
        };
        let waker = self.platform.waker();
        let cx = crate::origin::OriginCx {
            host: &host,
            app: &self.app,
            waker: &waker,
            now: self.platform.now(),
        };
        let first = host.next_ticket();
        let mut wrote = false;
        let origins: Vec<_> = self.origins.borrow().clone();
        for origin in &origins {
            if !origin.borrow().due() {
                continue;
            }
            let polled = origin.borrow_mut().poll(&cx);
            wrote |= polled.wrote;
            self.settle_due |= polled.settle;
        }
        if wrote {
            queue_commits(&host, &self.outgoing, first, None);
        }
        wrote
    }

    /// The package the instance runs.
    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }

    /// Ask the window to come forward ("Show this window"), with the next frame it is sent.
    pub fn activate_window(&mut self) {
        self.out.push(ToUi::Activate);
    }

    /// Have the window read in `locale` (`de-DE`), the language the instance's words now come in:
    /// its own chrome relabels with the next frame it is sent, and every view is drawn again.
    pub fn speak(&mut self, locale: String) {
        self.out.push(ToUi::Speak(locale));
        self.formats_changed = true;
    }

    /// What search hits and agents asked the window to show, until it shows them.
    #[must_use]
    pub fn open_requests(&self) -> Rc<RefCell<Vec<crate::effects::OpenEffect>>> {
        Rc::clone(&self.open_requests)
    }

    /// Show what the person's own step made, as a press of the action that made it does: it
    /// comes to the front as their input does, and takes the keyboard.
    pub fn opens_what_it_made(&mut self, anchor: String) {
        self.pressed_opens.push((self.taken_press, anchor));
    }

    /// Where the door's answers to effects mark work for the next frame.
    #[must_use]
    pub fn door_work(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.door_work)
    }

    /// The commits waiting for the door.
    #[must_use]
    pub fn outgoing(&self) -> Outgoing {
        Rc::clone(&self.outgoing)
    }

    /// The host, shared with whatever answers the door for it.
    #[must_use]
    pub fn shared_host(&self) -> Rc<RefCell<Option<Host>>> {
        Rc::clone(&self.host)
    }

    /// The host, once the instance has the record or its indexes.
    #[must_use]
    pub fn host(&self) -> Option<Host> {
        self.host.borrow().clone()
    }

    /// How many records the instance's publication has looked at so far, one per record per
    /// turn ([`Publication::visited`]).
    #[must_use]
    pub fn visited_records(&self) -> u64 {
        self.publication
            .as_ref()
            .map_or(0, |publication| publication.visited())
    }

    /// How many times the instance's publication has read every record in full
    /// ([`Publication::sweeps`]).
    #[must_use]
    pub fn sweeps(&self) -> u64 {
        self.publication
            .as_ref()
            .map_or(0, |publication| publication.sweeps())
    }

    /// How many times the window was sent what changed of its tree so far: one per frame whose
    /// change reached it (a tree's patches, or the rows of its lists), and the first tree after
    /// opening.
    #[must_use]
    pub fn patches_sent(&self) -> u64 {
        self.patches_sent
    }

    /// How long from the platform's clock now until the time a view of the window read
    /// (`today` in a query or a filter) next reads differently: the embedder wakes the instance
    /// then (a frame's deadline, its wait's timeout), and that frame runs again exactly the
    /// views that read it. Zero once it has. `None` when no view read the time: nothing about
    /// the time ever wakes the instance.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        [self.views_wake_in(), self.held_in(), self.trigger_in()]
            .into_iter()
            .flatten()
            .min()
    }

    /// How long until a trigger of this app next has something to run: what a window that runs
    /// the app's triggers waits for. `None` when no trigger will come due, or this replica does
    /// not run them.
    fn trigger_in(&self) -> Option<std::time::Duration> {
        if !self.origin || self.mode != Mode::Live {
            return None;
        }
        let now = self.now()?;
        let at = self.host()?.next_trigger(now)?;
        Some(std::time::Duration::from_secs(
            u64::try_from(at.saturating_sub(now)).unwrap_or(0),
        ))
    }

    /// The platform's clock now, in seconds since the epoch.
    fn now(&self) -> Option<i64> {
        let clock = self.platform.now();
        let now = tilolang::calendar::instant(&clock);
        if now.is_none() {
            tracing::error!(clock, "the platform's clock reads no time");
        }
        now
    }

    /// How long until the time a view read (`today` in a query) next reads differently.
    fn views_wake_in(&self) -> Option<std::time::Duration> {
        let at = [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|shown| shown.view.borrow().wakes_at())
            .min()?;
        let now = self.now()?;
        Some(std::time::Duration::from_secs(
            u64::try_from(at.saturating_sub(now)).unwrap_or(0),
        ))
    }

    /// How long until an outside effect an action held back (`outside(.., at:)`) is due: the
    /// frame then runs and the host carries it out. `None` when none is held back.
    fn held_in(&self) -> Option<std::time::Duration> {
        self.host()?.runtime().held_for()
    }

    /// The resource the root view shows.
    #[must_use]
    pub fn root_target(&self) -> Option<&ResourceRef> {
        self.root.as_ref().map(|view| &view.target)
    }

    /// The resource the detail view shows.
    #[must_use]
    pub fn detail_target(&self) -> Option<&ResourceRef> {
        self.detail.as_ref().map(|view| &view.target)
    }

    /// The root view's tree, as the runtime holds it.
    #[must_use]
    pub fn root_tree(&self) -> Option<UiNode> {
        self.root
            .as_ref()
            .map(|view| view.view.borrow().tree().clone())
    }

    /// The detail view's tree, as the runtime holds it.
    #[must_use]
    pub fn detail_tree(&self) -> Option<UiNode> {
        self.detail
            .as_ref()
            .map(|view| view.view.borrow().tree().clone())
    }

    /// The view of `lane`, to read it.
    #[must_use]
    pub fn view(&self, lane: Lane) -> Option<Rc<RefCell<ViewInstance>>> {
        self.lane(lane).map(|view| Rc::clone(&view.view))
    }

    /// The keys of the rows the view of `lane` renders now in its list `list`.
    #[must_use]
    pub fn shown_rows(&self, lane: Lane, list: &str) -> Vec<String> {
        let mut shown: Vec<String> = self
            .lane(lane)
            .and_then(|view| view.lists.get(list))
            .map(|rows| rows.shown().iter().cloned().collect())
            .unwrap_or_default();
        shown.sort();
        shown
    }

    /// The later build this instance took since last asked.
    pub fn take_taken_build(&mut self) -> Option<u64> {
        self.taken_build.take()
    }

    /// The undos and redos the person asked for since last asked, in the order they asked: Undo
    /// on a notice, and the window's undo and redo.
    pub fn take_history(&mut self) -> Vec<HistoryAsk> {
        std::mem::take(&mut self.history)
    }

    /// The next action the person chose at the window, with the press it is and the argument
    /// the palette collected: the host runs it as the person's own step, the way it runs any
    /// invocation of an action it publishes, and tells the instance it ran
    /// ([`Instance::press_ran`]). One a frame: what a press made opens, and the window hears
    /// it ran, before the press after it.
    pub fn take_chosen(&mut self) -> Option<(u64, ActionAddress, Option<String>)> {
        let chosen = self.chosen.pop_front();
        if let Some((press, _, _)) = &chosen {
            self.taken_press = *press;
        }
        chosen
    }

    /// Send the window `message` with this frame's batch: what the host knows of the actions it
    /// publishes for the window's person, which the instance does not build itself.
    pub fn say(&mut self, message: ToUi) {
        self.out.push(message);
    }

    /// The bound control the window's keyboard focus is in, and whether an input method is
    /// composing in it.
    #[must_use]
    pub fn focus(&self) -> (Option<&(Lane, String)>, bool) {
        (self.focus.as_ref(), self.composing)
    }

    /// Run the scenario `scenario` once the host runs, as a capture stages it.
    pub fn stage(&mut self, scenario: Option<&str>) {
        self.staged = scenario.map(str::to_owned);
        self.stage_failed = None;
    }

    /// Why the scenario [`stage`](Self::stage) named failed, or a package it uses failed its
    /// first scenario, once the host ran it: a capture refuses to picture a half-staged workspace.
    pub fn take_stage_failure(&mut self) -> Option<String> {
        self.stage_failed.take()
    }

    /// The workspace record moved: what the views read may have changed, and outside effects may
    /// wait.
    pub fn moved(&mut self) {
        self.moved = true;
    }

    /// The names of the workspace changed: views showing names show them again.
    pub fn names_changed(&mut self) {
        self.names_changed = true;
    }

    /// The person's formats changed: every view that writes a time or a day writes it again in
    /// the new style, and the window shows what it wrote.
    pub fn formats_changed(&mut self) {
        self.formats_changed = true;
    }

    /// Say whether this replica is the workspace's origin (the device that governs the workspace). Only
    /// the origin runs the package's migrations on what an earlier version wrote, so a migration
    /// runs once, where every change is ordered, and never as a reader elsewhere.
    pub fn set_origin(&mut self, origin: bool) {
        self.origin = origin;
    }

    fn lane(&self, lane: Lane) -> Option<&Shown> {
        match lane {
            Lane::Root => self.root.as_ref(),
            Lane::Detail => self.detail.as_ref(),
        }
    }

    fn lane_mut(&mut self, lane: Lane) -> Option<&mut Shown> {
        match lane {
            Lane::Root => self.root.as_mut(),
            Lane::Detail => self.detail.as_mut(),
        }
    }

    // ----- the host ------------------------------------------------------------------------

    /// The host over the workspace record `doc`, built with the package the first time; without the
    /// record (while it is read off the frame), over the workspace's indexes.
    fn host_for(&mut self, doc: Option<&tilo_resource::Doc>) -> Option<Host> {
        if let Some(host) = self.host.borrow().as_ref() {
            // An index built off the frame changes what every view reads, as a change does.
            let synced = doc.is_some_and(|doc| host.sync_from(doc));
            if synced | host.poll_index() {
                self.moved = true;
            }
            // Opened on what an earlier version wrote, the package brings it to where it keeps
            // it now, once the record is had; what they commit goes as any action does.
            if doc.is_some() && !self.migrated && self.origin && self.mode == Mode::Live {
                self.migrated = true;
                if host.migrate() > 0 {
                    self.moved = true;
                }
            }
            return Some(host.clone());
        }
        let index_dir = if self.mode == Mode::Live {
            self.index_dir.clone()
        } else {
            // A past version is read once and kept nowhere, and a capture shows its scenario
            // on a profile of its own: their indexes are built in memory, at once.
            None
        };
        let loader = match (doc, self.opening, &index_dir) {
            (Some(_), _, _) => None,
            (None, true, Some(_)) => Some(self.record_loader.take()?),
            // Without the record, a window opens over the indexes it keeps or not at all.
            (None, _, _) => return None,
        };
        let opening = Opening {
            record: doc,
            chunks: self.record_chunks.as_ref(),
            index_dir,
            device: self.device.as_deref(),
        };
        let standing = Rc::clone(&self.standing);
        let built = Host::open(opening, Principal::new(self.me.clone()), move || {
            standing.borrow().as_ref().and_then(|source| source())
        });
        let host = match built {
            Ok(host) => host,
            Err(error) => {
                tracing::error!(%error, "the tilolang runtime could not be set up");
                return None;
            }
        };
        if let Some(loader) = loader {
            // A write, or a field no index holds, waits for the record and lands on it.
            host.set_record_loader(Box::new(loader));
        }
        host.runtime()
            .set_name_host(SharedNames(Rc::clone(&self.names)));
        host.runtime()
            .set_shown_host(PlatformShown(Rc::clone(&self.platform)));
        let platform = Rc::clone(&self.platform);
        host.set_clock(move || platform.now());
        host.runtime()
            .set_web(PlatformWeb(Rc::clone(&self.platform)));
        host.runtime()
            .set_blobs(PlatformBlobs(Rc::clone(&self.platform)));
        let platform = Rc::clone(&self.platform);
        host.set_zone(move || platform.zone());
        let platform = Rc::clone(&self.platform);
        host.runtime().set_zone_name(move || platform.zone_name());
        let platform = Rc::clone(&self.platform);
        host.runtime().set_week_start(move || platform.week_start());
        host.set_words(PlatformWords(Rc::clone(&self.platform)));
        if let Some(sink) = self.sink.take() {
            host.runtime().set_sink(crate::origin::RoutingSink {
                origins: Rc::clone(&self.origins),
                rest: sink,
            });
        }
        if self.mode != Mode::Past {
            // The app's index is owed a frame as soon as it is built: the window shows what it
            // can until the package's load has it, which builds it off the frame.
            host.set_index_waker(self.platform.waker());
        }
        self.generation = self.platform.load(&host);
        if self.platform.follows_updates() && self.mode != Mode::Past && !self.follows {
            self.follows = true;
            self.platform.follow_updates();
        }
        if self.mode != Mode::Past {
            // What is published and found of the workspace is read off the frame the first time;
            // the instance takes it in when it is ready.
            self.publication = self.platform.publish(&host, self.platform.waker());
        }
        *self.host.borrow_mut() = Some(host.clone());
        Some(host)
    }

    /// The name a bound control is announced by: the field it edits, through what the host
    /// declares of that field — the field's own word in the window's language, and its declared
    /// name in words while the package names it in no catalogue.
    fn field_name(host: &Host, binding: &Binding) -> String {
        announced(binding, |target, field| {
            type_of(host, &target.ty)
                .and_then(|(_, ty)| {
                    ty.fields
                        .iter()
                        .find(|declared| declared.id == field)
                        .map(|declared| {
                            host.runtime().words().word(
                                &tilolang::messages::key_of(&declared.id),
                                &[],
                                &sentence(&declared.name),
                            )
                        })
                })
                .unwrap_or_default()
        })
    }

    // ----- later builds --------------------------------------------------------------------

    /// Take over the latest later build of the package, when this instance runs an older one:
    /// in place when it reads the state the old one wrote, and otherwise by starting over on
    /// the workspace record `doc` with it, as the app's next start would. The host the instance runs
    /// on afterwards.
    fn follow_updates(&mut self, doc: &tilo_resource::Doc, host: Host) -> Host {
        let Some(update) = self
            .platform
            .latest()
            .filter(|update| update.generation > self.generation && self.mode != Mode::Past)
        else {
            return host;
        };
        self.generation = update.generation;
        let Some(old) = host.package(&self.app) else {
            return host;
        };
        let title = old.meta.title.clone();
        let host = match crate::upgrade::compare(&old.meta, &update.bundle.meta) {
            Upgrade::InPlace => match host.load_bundle(&update.bundle) {
                Ok(_) => {
                    self.reopen(&host);
                    tracing::info!("tilolang: {title} runs the new build, with its state");
                    host
                }
                Err(error) => {
                    tracing::warn!(
                        "tilolang: {title} keeps the last build: the new one does not load: {error}"
                    );
                    return host;
                }
            },
            Upgrade::Restart(reason) => {
                tracing::info!(
                    "tilolang: {title}: {reason}; it starts over on the new build, with its data"
                );
                self.start_over(doc).unwrap_or(host)
            }
        };
        self.platform.surface_changed(&self.app, &update.bundle);
        self.taken_build = Some(update.generation);
        host
    }

    /// Open the root and the detail again on the package `host` now runs: the same resources,
    /// so the same windows and their view state.
    fn reopen(&mut self, host: &Host) {
        let detail = self
            .detail
            .as_ref()
            .map(|view| (view.target.clone(), view.with.clone()));
        let (Ok(target), Ok(view)) = (
            host.root_of_layer(&self.app, &self.root_view),
            host.open_layer(&self.app, &self.root_view),
        ) else {
            tracing::warn!(app = self.app, "the new build has no root to open");
            return;
        };
        self.root = Some(Shown::new(target, self.spaced(Lane::Root, view)));
        self.send_open(Lane::Root, None);
        self.detail = detail.and_then(|(target, with)| {
            let view = host.open_resource(&target, with.as_deref()).ok()?;
            let mut view = Shown::new(target, self.spaced(Lane::Detail, view));
            view.with = with;
            Some(view)
        });
        if self.detail.is_some() {
            self.send_open(Lane::Detail, None);
        }
        self.lenses_changed = true;
        // What is published and offered follows the new build's declarations.
        if self.publication.take().is_some() {
            self.publication = self.platform.publish(host, self.platform.waker());
        }
        self.commit_planned();
    }

    /// Start over on the workspace record `doc`: a new runtime that loads the latest build and reads
    /// the record afresh, as a start of the app does. View state and the open detail start
    /// over; the record, and so every resource, is the one the instance had.
    fn start_over(&mut self, doc: &tilo_resource::Doc) -> Option<Host> {
        self.publication = None;
        self.root = None;
        if self.detail.take().is_some() {
            self.out.push(ToUi::Close);
        }
        *self.host.borrow_mut() = None;
        if self.sink.is_none() {
            self.sink = Some(Box::new(crate::sink::LogSink::dev()));
        }
        self.started = false;
        self.host_for(Some(doc))
    }

    // ----- presence ------------------------------------------------------------------------

    /// Bring the lenses the views read up to date with presence: who else is at each resource
    /// (`peers`, by resource id), and the person themselves at the detail view's resource.
    /// Someone composing a message in the detail view is a typist there: `composers` names
    /// them, and their lens reads `composing` (`lenses(c)`), so a conversation can say
    /// "… is typing" without reading a draft.
    pub fn set_peers(&mut self, peers: HashMap<String, String>) {
        self.set_peers_and_composers(peers, &[]);
    }

    /// [`Self::set_peers`], with who is composing a message in the detail view's resource.
    pub fn set_peers_and_composers(
        &mut self,
        peers: HashMap<String, String>,
        composers: &[String],
    ) {
        let mut now: HashMap<String, Vec<(Principal, bool, bool)>> = HashMap::new();
        for (resource, who) in peers {
            let typing = composers.contains(&who);
            now.entry(resource)
                .or_default()
                .push((Principal::new(who), false, typing));
        }
        if let Some(detail) = &self.detail {
            now.entry(detail.target.id_string()).or_default().push((
                Principal::new(self.me.clone()),
                true,
                false,
            ));
        }
        if now == self.lenses {
            return;
        }
        if let Some(host) = self.host() {
            for gone in self.lenses.keys().filter(|key| !now.contains_key(*key)) {
                host.runtime().set_lenses(gone, &[]);
            }
            for (resource, lenses) in &now {
                if self.lenses.get(resource) != Some(lenses) {
                    host.runtime().set_lenses(resource, lenses);
                }
            }
        }
        self.lenses = now;
        self.lenses_changed = true;
    }

    // ----- staging -------------------------------------------------------------------------

    /// Run the scenario a capture stages: its hidden action fills the workspace.
    fn run_staged(&mut self, host: &Host) {
        let Some(name) = self.staged.take() else {
            return;
        };
        let Some(scenario) = host.package(&self.app).and_then(|package| {
            package
                .meta
                .scenarios
                .iter()
                .find(|scenario| scenario.name == name)
                .cloned()
        }) else {
            self.stage_failed = Some(format!("{} has no scenario \"{name}\"", self.app));
            return;
        };
        // The packages this one points into (a board's apps) are staged first, each with its
        // first scenario, so a scenario that attaches their roots shows them filled.
        let mut failed = Vec::new();
        for used in used_packages(host, &self.app) {
            if let Some(first) = used.meta.scenarios.first()
                && let Err(why) = self.run_scenario(host, &used.meta.app, &first.action)
            {
                failed.push(format!("{} \"{}\": {why}", used.meta.app, first.name));
            }
        }
        let shown = match self.run_scenario(host, &self.app.clone(), &scenario.action) {
            Ok(shown) => shown,
            Err(why) => {
                failed.push(format!("{} \"{name}\": {why}", self.app));
                None
            }
        };
        if !failed.is_empty() {
            self.stage_failed = Some(failed.join("; "));
        }
        self.commit_planned();
        // A scenario that returns a resource shows it, as a primary action shows what it made:
        // in place in the list of the place it belongs to, when the view lists its type. A
        // script opened it, so a narrow window brings it in front at once.
        if let Some(shown) = shown {
            self.show_in_window(&shown, Some(ui::Front::Action), 0);
        }
    }

    /// Run a scenario (its hidden action `action`) of the package `app` over its root: the
    /// resource it returned, when it returns one, or why it did not run whole.
    fn run_scenario(
        &self,
        host: &Host,
        app: &str,
        action: &str,
    ) -> Result<Option<ResourceRef>, String> {
        host.root_of(app)
            .map_err(|error| format!("the package has no root to stage: {error}"))?;
        match host
            .runtime()
            .invoke_action(&Principal::new(self.me.clone()), (action, Vec::new()))
        {
            Ok(InvokeOutcome::Done(Value::Ref(shown))) => {
                Ok(Some(Rc::unwrap_or_clone(shown.target)))
            }
            Ok(InvokeOutcome::Done(_)) => Ok(None),
            Ok(other) => Err(format!("it did not run: {other:?}")),
            Err(error) => Err(error.to_string()),
        }
    }

    // ----- the door ------------------------------------------------------------------------

    fn notice(&mut self, words: Words, kind: ui::NoticeKind) {
        // The window shows one notice at a time: what the one before offered to take back is
        // gone with it.
        self.seen_offer = false;
        self.out.push(ToUi::Notice {
            words,
            kind,
            undo: false,
        });
    }

    /// The record's changes stay on this device, for `why` (the workspace refused them for good):
    /// the window says so.
    pub fn unshared(&mut self, why: String) {
        self.notice(Words::Plain(why), ui::NoticeKind::Error);
        self.flush();
    }

    /// The undo or redo the person asked for failed, for `why` (an inverse could not be applied,
    /// or the workspace refused the change for good): the window says so. Somebody else having changed
    /// the record since is never a failure: undo leaves their newer value and takes back the rest.
    pub fn history_failed(&mut self, why: String) {
        self.notice(Words::Plain(why), ui::NoticeKind::Error);
        self.flush();
    }

    /// The action the person chose from the window's command palette did not run, for `why`
    /// (the app refused it, or it was no longer offered): the window says so.
    pub fn action_refused(&mut self, why: String) {
        self.notice(Words::Plain(why), ui::NoticeKind::Error);
        self.flush();
    }

    fn take_door_work(&mut self) -> bool {
        if !self.door_work.take() {
            return false;
        }
        self.commit_planned();
        if let Some(host) = self.host()
            && let Some(suggestion) = host.take_suspended()
        {
            self.ask_for(suggestion);
        }
        true
    }

    /// Show the Ask bar for `suggestion`.
    fn ask_for(&mut self, suggestion: Suggestion) {
        let action = self
            .host()
            .and_then(|host| {
                host.find_in_packages(|package| {
                    package
                        .meta
                        .actions
                        .iter()
                        .find(|action| action.id == suggestion.action)
                        .map(crate::runtime::platform::label_of)
                })
            })
            .unwrap_or_else(|| suggestion.action.clone());
        self.out.push(ToUi::Ask(Some(ui::Ask {
            action,
            invoker: suggestion.invoker.0.clone(),
        })));
        self.ask = Some(suggestion);
    }

    /// Show what a person's press made, and what search hits or agents asked the window to
    /// show.
    fn take_open_requests(&mut self) -> bool {
        // What a press made comes to the front as the person's input does, and takes the
        // keyboard; a search hit or an agent's `open` moved no hand, and lands at once.
        let anchors: Vec<(crate::effects::OpenEffect, ui::Front, u64)> =
            std::mem::take(&mut self.pressed_opens)
                .into_iter()
                .map(|(press, anchor)| {
                    (
                        crate::effects::OpenEffect::Open { anchor },
                        ui::Front::Input,
                        press,
                    )
                })
                .chain(
                    std::mem::take(&mut *self.open_requests.borrow_mut())
                        .into_iter()
                        .map(|open| (open, ui::Front::Action, self.last_fire)),
                )
                .collect();
        if anchors.is_empty() {
            return false;
        }
        let Some(host) = self.host() else {
            return false;
        };
        for (open, front, press) in anchors {
            // A hit names the published resource (`tl:<app>:<id>`) or the resource's id.
            let anchor = open.anchor();
            let anchor = anchor
                .strip_prefix("tl:")
                .and_then(|rest| rest.split_once(':'))
                .map_or(anchor, |(_, id)| id);
            match host.open_plan(anchor, open.view()) {
                Ok(plan) => {
                    // The app's own root in its default view is what the window shows already.
                    if let Some(target) = plan.target
                        && (target.item.is_some() || plan.app != self.app || plan.view.is_some())
                    {
                        match plan.view.as_deref() {
                            None => self.show_in_window(&target, Some(front), press),
                            Some(view) => self.open_detail(&target, Some(view), Some(front)),
                        }
                    }
                    self.commit_planned();
                }
                Err(error) => {
                    tracing::warn!(%error, "a search hit named nothing in the workspace");
                    self.notice(Words::phrase("tilolang-open-failed"), ui::NoticeKind::Error);
                }
            }
        }
        true
    }

    fn settle_outbox(&mut self) -> bool {
        // What waits in the record is settled once the instance reads the record.
        if self.opening || !std::mem::take(&mut self.settle_due) || self.mode != Mode::Live {
            return false;
        }
        let Some(host) = self.host() else {
            return false;
        };
        let first = host.next_ticket();
        match host.settle_outbox() {
            Ok(settled) => {
                // What was marked as being handed over is handed over by the next settle,
                // which nothing else would ask for.
                self.settle_due |= settled.owes_settle;
                if settled.written == 0 {
                    return false;
                }
                queue_commits(&host, &self.outgoing, first, None);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "outside effects could not be settled");
                false
            }
        }
    }

    /// Queue what the runtime committed for the door. Returns the tickets queued.
    pub fn commit_planned(&mut self) -> Vec<u64> {
        // A past version shows what it held; nothing it does is committed.
        if self.mode == Mode::Past {
            return Vec::new();
        }
        let Some(host) = self.host() else {
            return Vec::new();
        };
        let tickets = queue_commits(&host, &self.outgoing, 0, None);
        self.settle_due = true;
        tickets
    }

    /// The next invocation for the door, with the invocation of the door that made it.
    pub fn take_invocation(&mut self) -> Option<(ControlInvocation, Option<InvocationId>)> {
        self.take_door_work();
        let (invocation, origin) = self.outgoing.borrow_mut().pop_front()?;
        if invocation.address.resource == HOST_RESOURCE {
            let ticket = invocation
                .arguments
                .get("ticket")
                .and_then(Json::as_u64)
                .unwrap_or_default();
            self.in_flight.push_back(ticket);
        }
        Some((invocation, origin))
    }

    /// The door answered an invocation the instance handed over: done, or refused with
    /// `error`. Whether it was the instance's own.
    pub fn answered(&mut self, address: &ActionAddress, error: Option<String>) -> bool {
        if address.resource == HOST_RESOURCE && address.action == COMMIT {
            self.door_answered(error);
            self.flush();
            return true;
        }
        if let Some(error) = error
            && address.resource.starts_with("tl:")
        {
            self.notice(Words::Plain(error), ui::NoticeKind::Error);
            self.flush();
            return true;
        }
        false
    }

    /// The door answered the oldest commit handed over.
    fn door_answered(&mut self, error: Option<String>) {
        let Some(ticket) = self.in_flight.pop_front() else {
            return;
        };
        if let Some(host) = self.host() {
            let _ = host.store().take_ticket(ticket);
        }
        let undo = self.pending.undo.remove(&ticket);
        match (error, undo) {
            (Some(error), _) => self.notice(Words::Plain(error), ui::NoticeKind::Error),
            (None, Some(words)) => {
                // A destructive action is undone rather than confirmed.
                self.seen_offer = false;
                self.out.push(ToUi::Notice {
                    words,
                    kind: ui::NoticeKind::Info,
                    undo: true,
                });
            }
            (None, None) => {}
        }
    }

    // ----- what the person did -------------------------------------------------------------

    /// Take what the window sent and do it. Whether anything was.
    fn receive(&mut self) -> bool {
        let mut changed = false;
        loop {
            let batch = match self.transport.recv() {
                Ok(Some(batch)) => batch,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "the window's connection failed");
                    break;
                }
            };
            let messages: Vec<ToApp> = match ui::decode_batch(&batch) {
                Ok(messages) => messages,
                Err(error) => {
                    tracing::warn!(%error, "the window sent what is no batch");
                    continue;
                }
            };
            for message in messages {
                changed = true;
                self.event(message);
            }
        }
        changed
    }

    /// The view of `lane` has `width` by `height` rem of the window now: its `space` follows,
    /// with a class changing only past its boundary by the hysteresis, and the view runs again
    /// only when something it read of it changed.
    fn offer_space(&mut self, lane: Lane, width: f64, height: f64) {
        let slot = match lane {
            Lane::Root => &mut self.root_space,
            Lane::Detail => &mut self.detail_space,
        };
        let space = slot.map_or_else(
            || crate::runtime::space::Space::from_rem(width, height),
            |before| before.followed(width, height),
        );
        *slot = Some(space);
        if let Some(view) = self.view(lane) {
            view.borrow_mut().set_space("", space);
        }
    }

    /// `view`, about to be shown in `lane`, offered the space the lane has.
    fn spaced(&self, lane: Lane, mut view: ViewInstance) -> ViewInstance {
        let space = match lane {
            Lane::Root => self.root_space,
            Lane::Detail => self.detail_space,
        };
        if let Some(space) = space {
            view.set_space("", space);
        }
        view
    }

    /// Take in what the window tells of itself and of the person at it, which runs nothing
    /// there and then: where the keyboard is, whether an input method composes, what it calls
    /// people, the space a view has, and the undos and redos asked for, which the one who runs
    /// the instance carries out on the record ([`Self::take_history`]).
    fn window_told(&mut self, told: ToApp) {
        match told {
            // The notice in front takes back what it reported: the marks the person just moved
            // seen, where it was that notice, else the record's last change.
            ToApp::Undo => {
                let taken = std::mem::take(&mut self.seen_offer)
                    && self
                        .host()
                        .is_some_and(|host| host.runtime().undo_seen(&self.me));
                if !taken {
                    self.history.push(HistoryAsk::Undo);
                }
            }
            ToApp::Redo => self.history.push(HistoryAsk::Redo),
            ToApp::Focus { at } => self.focus = at,
            ToApp::Composing(composing) => self.composing = composing,
            ToApp::Names { me, others } => {
                if self.told.tell(me, others) {
                    self.names_changed = true;
                }
            }
            ToApp::Space {
                lane,
                width,
                height,
            } => self.offer_space(lane, f64::from(width), f64::from(height)),
            other => tracing::error!(?other, "the window told of something it does not"),
        }
    }

    /// Carry out `message`, one thing the window told of the person at it, as a step of theirs,
    /// and send what followed it: what a press made, and what the window hears after a press.
    fn event(&mut self, message: ToApp) {
        let first = self.host().map(|host| host.next_ticket());
        let mut ran: Option<(ActionRef, Value)> = None;
        let pressed = matches!(message, ToApp::Fire { .. });
        let outcome = self.carry_out(message, &mut ran);
        if let Err(words) = outcome {
            self.notice(words, ui::NoticeKind::Error);
            // What the controls show goes back to what they are bound to.
            for lane in [Lane::Root, Lane::Detail] {
                if let Some(view) = self.lane_mut(lane) {
                    view.bound.clear();
                }
                self.send_bound(lane);
            }
        }
        let tickets = self.commit_planned();
        if let (Some((aref, result)), Some(first)) = (ran, first) {
            self.after_action(&aref, &result, tickets.into_iter().filter(|t| *t >= first));
        }
        self.follow_opens();
        if pressed {
            // What the press made opens now, and the window hears right after it that the press
            // ran: what the person typed after it goes to what it opened, never to what a later
            // press of the same batch opens.
            // What it opened is drawn before what the person did after the press reaches the
            // view: the page beside the list shows the item the press opened, and what they
            // type into it is that item's.
            self.take_open_requests();
            self.poll_views();
            self.press_ran(self.last_fire);
            self.tell_took();
        }
    }

    /// Carry out what the window told `self` of, answering what it could not do in words; `ran`
    /// takes the action it applied and what that returned, when it ran one.
    fn carry_out(
        &mut self,
        message: ToApp,
        ran: &mut Option<(ActionRef, Value)>,
    ) -> Result<(), Words> {
        match message {
            ToApp::Back => {
                self.close_detail();
                Ok(())
            }
            ToApp::Fire {
                lane,
                key,
                event,
                value,
            } => self.fire(lane, &key, &event, value.as_ref(), ran),
            edit @ (ToApp::Toggle { .. }
            | ToApp::Splice { .. }
            | ToApp::Edit { .. }
            | ToApp::EditRich { .. }) => self.edit_event(edit),
            ToApp::Resolve { lane, key, approve } => self.resolve(lane, &key, approve),
            ToApp::Open { resource } => {
                self.show_now(&resource);
                Ok(())
            }
            ToApp::OpenReference { reference } => {
                self.open_reference(&reference);
                Ok(())
            }
            ToApp::Rows {
                lane,
                list,
                start,
                end,
                open,
            } => {
                self.request_rows(lane, &list, start as usize..end as usize, open.as_deref());
                Ok(())
            }
            ToApp::Answer(answer) => {
                self.answer(answer);
                Ok(())
            }
            ToApp::FormEdit { form, field, text } => {
                self.form_edit(form, &field, text);
                Ok(())
            }
            ToApp::FormSubmit { form } => self.form_submit(form, ran),
            ToApp::FormCancel { form } => {
                if self.form.as_ref().is_some_and(|open| open.id == form) {
                    self.form = None;
                    self.out.push(ToUi::Form(None));
                }
                Ok(())
            }
            told @ (ToApp::Undo
            | ToApp::Redo
            | ToApp::Focus { .. }
            | ToApp::Composing(_)
            | ToApp::Names { .. }
            | ToApp::Space { .. }) => {
                self.window_told(told);
                Ok(())
            }
            ToApp::Invoke { address, argument } => {
                // The window's own bar, its palette and anyone else took an action of this
                // window: a press, which the host runs as the person's own step, so it holds
                // the address and the argument until its next frame. A command that still needs
                // arguments opens its form first, the one way every action asks for what it was
                // not given.
                let fired = self.fired.saturating_add(1);
                self.fired = fired;
                self.last_fire = fired;
                if argument.is_none() && self.open_chosen_form(&address) {
                    Ok(())
                } else {
                    self.chosen.push_back((fired, address, argument));
                    Ok(())
                }
            }
        }
    }

    /// The press `at` ran: what it made was sent before, or is sent by the frame that followed
    /// it. The window hears how many presses ran in order, so what a person typed after a
    /// press never goes to one still to run.
    pub fn press_ran(&mut self, at: u64) {
        if at != self.ran.saturating_add(1) {
            if at > self.ran {
                self.ran_ahead.insert(at);
            }
            return;
        }
        self.ran = at;
        while self.ran_ahead.remove(&self.ran.saturating_add(1)) {
            self.ran = self.ran.saturating_add(1);
        }
    }

    /// Show what the presses the host ran this frame made ([`Self::press_ran`]) once they are
    /// in the record: what they opened, drawn, and the window told they ran, in the frame that
    /// ran them rather than the next, so the window takes all of it in one frame and what the
    /// person typed after a press goes to what it opened.
    pub fn show_ran(&mut self) {
        self.take_open_requests();
        self.poll_views();
        self.tell_took();
        self.flush();
    }

    /// Tell the window how many of its presses ran, when that changed: what they opened was
    /// sent before.
    fn tell_took(&mut self) {
        if self.ran != self.fired_told {
            self.fired_told = self.ran;
            self.out.push(ToUi::Took { fired: self.ran });
        }
    }

    /// Show the resource `resource` a component was activated for, in front of the window.
    fn show_now(&mut self, resource: &tilo_render_protocol::patch::ResourceRef) {
        if let Some(target) = wire::runtime_resource(resource) {
            self.show_in_window(&target, Some(ui::Front::Input), self.last_fire);
        }
    }

    /// Render the rows `range` of the list `list` of the view of `lane` for the window, and the
    /// document of the row `open` — the row standing open into its item where it stands, which
    /// the window shows and no other row does.
    fn request_rows(
        &mut self,
        lane: Lane,
        list: &str,
        range: std::ops::Range<usize>,
        open: Option<&str>,
    ) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.lane_mut(lane) else {
            return;
        };
        let handle = Rc::clone(&view.view);
        let Some(rows) = view.lists.get_mut(list) else {
            return;
        };
        let requested = rows.request(range, open);
        let message = rows_message(&host, &*names, &handle, lane, list, requested);
        self.out.push(message);
        // The rows rendered can hold lists of their own, and those scrolled away took theirs.
        self.track_lists(lane);
        self.send_bound(lane);
    }

    /// Fire the event `name` of the node `key` of the view of `lane`, carrying `value`; `ran`
    /// takes the action it applied and what that returned, when it ran one.
    fn fire(
        &mut self,
        lane: Lane,
        key: &str,
        name: &str,
        value: Option<&ui::Value>,
        ran: &mut Option<(ActionRef, Value)>,
    ) -> Result<(), Words> {
        // Counted whatever comes of it: the window waits for every press it sent.
        self.fired = self.fired.saturating_add(1);
        self.last_fire = self.fired;
        let Some(view) = self.view(lane) else {
            return Ok(());
        };
        let value = value.and_then(wire::runtime_value);
        // What a drag from another view carried reaches this view by its published address and
        // nothing more (a chip dropped on a composer, a place's handle dropped on the canvas):
        // its type is read here, from the record this workspace holds or the reading its app
        // published, so the action the drop runs takes the resource it names. An address
        // neither knows yet keeps what it arrived as, and an action taking it as any resource
        // (`host.Resource`) still runs.
        let value = value.map(|value| self.resolved(value));
        let wanting = view
            .borrow()
            .wanting_arguments(key, name, value.as_ref())
            .map_err(|error| Words::Plain(error.to_string()))?;
        if let Some(aref) = wanting
            && self.open_form(lane, key, Rc::clone(&view), aref)
        {
            return Ok(());
        }
        let action = view.borrow().event_action(key, name);
        let fired = view.borrow().fire_with(key, name, value.as_ref());
        match fired {
            Ok(InvokeOutcome::Done(result)) => {
                *ran = action.map(|action| (action, result));
                Ok(())
            }
            Ok(InvokeOutcome::Suggested(suggestion)) => {
                self.ask_for(suggestion);
                Ok(())
            }
            Ok(InvokeOutcome::Denied) => Err(Words::phrase("tilolang-ask-denied")),
            Err(error) => Err(Words::Plain(error.to_string())),
        }
    }

    /// `value` with every reference in it naming its type: a resource a drag from another view
    /// carried reached this one by its published address, and its type is read from the
    /// workspace's record, else from the reading its app published ([`Runtime::resolve_ref`]).
    /// A resource that already names its type, and a value that holds none, is returned as it
    /// is.
    fn resolved(&self, value: Value) -> Value {
        let Some(host) = self.host() else {
            return value;
        };
        let resolve =
            |target: &ResourceRef| -> Option<ResourceRef> { host.runtime().resolve_ref(target) };
        resolved_value(value, &resolve)
    }

    /// Show the form of `aref`, which the control `key` of the view of `lane` ran and which
    /// still needs arguments, instead of running it. Whether it has one: what it needs is
    /// what a form can ask for.
    fn open_form(
        &mut self,
        lane: Lane,
        key: &str,
        view: Rc<RefCell<ViewInstance>>,
        aref: ActionRef,
    ) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let Some(meta) = host
            .packages()
            .into_iter()
            .map(|package| package.meta)
            .find(|meta| meta.actions.iter().any(|action| action.id == aref.action))
        else {
            return false;
        };
        self.open_form_with(Some((lane, key.to_owned())), view, aref, meta)
    }

    /// Show the form of `aref`, an action of `meta` waiting for arguments a form asks for,
    /// standing where `from` names the control that ran it, or on its own.
    fn open_form_with(
        &mut self,
        from: Option<(Lane, String)>,
        view: Rc<RefCell<ViewInstance>>,
        aref: ActionRef,
        meta: Meta,
    ) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let Some(pending) =
            form::Pending::new(self.forms.saturating_add(1), from, view, aref, meta)
        else {
            return false;
        };
        self.forms = pending.id;
        self.out.push(ToUi::Form(Some(Box::new(
            pending.shown(&host, &*self.names),
        ))));
        self.form = Some(pending);
        true
    }

    /// Show the form of the action `address` names, which the window's bar or the palette ran,
    /// when it still needs arguments a form asks for: the action as the record it is bound to
    /// gives it. Whether it opened one.
    fn open_chosen_form(&mut self, address: &ActionAddress) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let Some(meta) = host
            .packages()
            .into_iter()
            .map(|package| package.meta)
            .find(|meta| {
                meta.actions
                    .iter()
                    .any(|action| action.id == address.action)
            })
        else {
            return false;
        };
        let Some(action) = meta
            .actions
            .iter()
            .find(|action| action.id == address.action)
            .cloned()
        else {
            return false;
        };
        // What the address names the action of, when the action's first parameter is its
        // subject: the resource the descriptor was published on.
        let mut args = Vec::new();
        if let Some(subject) = meta.subject_of(&action.params) {
            // The address names the resource as the door publishes it (`tl:<app>:<id>`); the
            // runtime holds it by its id.
            let id = address
                .resource
                .strip_prefix(&crate::publish::resource_id(&meta.app, ""))
                .unwrap_or(&address.resource);
            let Some(target) = ResourceRef::parse_id(id, subject.id.clone()) else {
                return false;
            };
            args.push(Value::resource(&target));
        }
        // What is still to ask is what the action has no default for, as the label's "…" and
        // every form read it: when the address's subject leaves nothing, the action runs as it
        // is, as a control that runs it runs it.
        if action.unbound(args.len()).next().is_none() {
            return false;
        }
        let Some(view) = self.view(Lane::Root) else {
            return false;
        };
        let aref = ActionRef {
            action: action.id,
            args,
            author: None,
            from: None,
        };
        self.open_form_with(None, Rc::clone(&view), aref, meta)
    }

    /// The field `field` of the form `id` now holds `text`: kept, and the form shown as it
    /// reads now.
    fn form_edit(&mut self, id: u64, field: &str, text: String) {
        let Some(host) = self.host() else {
            return;
        };
        let Some(pending) = self.form.as_mut().filter(|open| open.id == id) else {
            return;
        };
        pending.edit(field, text);
        self.out.push(ToUi::Form(Some(Box::new(
            pending.shown(&host, &*self.names),
        ))));
    }

    /// The form `id` was submitted: run its action with what it holds, once. A refusal shows in
    /// the form, which stays open.
    fn form_submit(&mut self, id: u64, ran: &mut Option<(ActionRef, Value)>) -> Result<(), Words> {
        let Some(host) = self.host() else {
            return Ok(());
        };
        let Some(pending) = self.form.as_mut().filter(|open| open.id == id) else {
            return Ok(());
        };
        let aref = match pending.completed(&host) {
            Ok(aref) => aref,
            Err((_, why)) => {
                pending.error = Some(why);
                self.out.push(ToUi::Form(Some(Box::new(
                    pending.shown(&host, &*self.names),
                ))));
                return Ok(());
            }
        };
        let outcome = pending.view.borrow().invoke(&aref);
        match outcome {
            Ok(InvokeOutcome::Done(result)) => {
                pending.forget();
                self.form = None;
                self.out.push(ToUi::Form(None));
                *ran = Some((aref, result));
                Ok(())
            }
            Ok(InvokeOutcome::Suggested(suggestion)) => {
                pending.forget();
                self.form = None;
                self.out.push(ToUi::Form(None));
                self.ask_for(suggestion);
                Ok(())
            }
            Ok(InvokeOutcome::Denied) => Err(Words::phrase("tilolang-ask-denied")),
            Err(error) => {
                pending.error = Some(refusal(&error));
                self.out.push(ToUi::Form(Some(Box::new(
                    pending.shown(&host, &*self.names),
                ))));
                Ok(())
            }
        }
    }

    /// What the person typed or toggled in a bound control, applied to the resource they did it
    /// in.
    fn edit_event(&mut self, message: ToApp) -> Result<(), Words> {
        match message {
            ToApp::Toggle {
                lane,
                target,
                key,
                checked,
            } => self.edit(lane, &target, &key, Value::Bool(checked)),
            ToApp::Splice {
                lane,
                target,
                key,
                seq,
                at,
                delete,
                insert,
            } => self.splice(lane, &target, &key, seq, (at, delete), &insert),
            ToApp::Edit {
                lane,
                target,
                key,
                text,
            } => self.edit(lane, &target, &key, Value::Text(text)),
            ToApp::EditRich {
                lane,
                target,
                key,
                rich,
            } => {
                let rich = Value::Rich(wire::runtime_rich(&rich));
                self.edit(lane, &target, &key, rich)
            }
            _ => Ok(()),
        }
    }

    /// Where an edit the window made in the view of `lane` showing `target` applies.
    fn edited(&self, lane: Lane, target: &ui::ResourceRef) -> Result<Place, Words> {
        if let Some(shown) = self.lane(lane) {
            // The lane's own view, showing the item the edit was made in.
            if wire::resource(&shown.target) == *target {
                return Ok(Place::Here(Rc::clone(&shown.view)));
            }
            // The window draws its page beside its list for the item it shows: the edit is the
            // item's, and the page shows it again once the edit landed.
            if let Some(before) = &shown.in_place
                && wire::resource(before) != *target
            {
                return Ok(Place::Beside {
                    view: Rc::clone(&shown.view),
                    before: before.clone(),
                });
            }
            if shown.in_place.is_some() {
                return Ok(Place::Here(Rc::clone(&shown.view)));
            }
        }
        let open_failed = || Words::phrase("tilolang-open-failed");
        let host = self.host().ok_or_else(open_failed)?;
        let resource = wire::runtime_resource(target).ok_or_else(open_failed)?;
        let view = host.open_resource(&resource, None).map_err(|error| {
            tracing::warn!(%error, "the resource an edit was made in did not open");
            open_failed()
        })?;
        Ok(Place::Fresh(Rc::new(RefCell::new(view))))
    }

    /// Apply a splice of the bound text control `key` of the view of `target` in `lane`:
    /// `delete` bytes at `at` replaced by `insert`, against the text it shows.
    fn splice(
        &mut self,
        lane: Lane,
        target: &ui::ResourceRef,
        key: &str,
        seq: u64,
        (at, delete): (u32, u32),
        insert: &str,
    ) -> Result<(), Words> {
        let place = self.edited(lane, target)?;
        if place.shown()
            && let Some(lane) = self.lane_mut(lane)
        {
            let acked = lane.acked.entry(key.to_owned()).or_default();
            *acked = (*acked).max(seq);
        }
        let current = match place.view().borrow().bound_value(key) {
            Some(Value::Text(text)) => text,
            _ => String::new(),
        };
        let text = spliced(&current, at as usize, delete as usize, insert);
        self.edit_at(lane, target, place, key, Value::Text(text))
    }

    /// Apply an edit of the bound control `key` of the view of `target` in `lane`.
    fn edit(
        &mut self,
        lane: Lane,
        target: &ui::ResourceRef,
        key: &str,
        value: Value,
    ) -> Result<(), Words> {
        let place = self.edited(lane, target)?;
        self.edit_at(lane, target, place, key, value)
    }

    /// Apply the edit of the bound control `key` made in the view of `target` in `lane`, where
    /// the window's page draws `target` or another item: the page shows the item while the edit
    /// lands, and the window shows again the item it showed, with what the edit made.
    fn edit_at(
        &mut self,
        lane: Lane,
        target: &ui::ResourceRef,
        place: Place,
        key: &str,
        value: Value,
    ) -> Result<(), Words> {
        let shown = place.shown();
        let beside = match &place {
            Place::Beside { before, .. } => Some(before.clone()),
            _ => None,
        };
        if beside.is_some()
            && let Some(item) = wire::runtime_resource(target)
        {
            let view = place.view();
            if let Err(error) = view.borrow().show(&item) {
                tracing::warn!(%error, "the item an edit was made in did not show");
                return Err(Words::phrase("tilolang-open-failed"));
            }
            // Drawn, so the fields are the item's own: what the edit names is in its page.
            if let Err(error) = view.borrow_mut().poll_patches() {
                tracing::warn!(%error, "the view an edit was made in did not run");
                return Err(Words::phrase("tilolang-open-failed"));
            }
            let view = place.into_view();
            let outcome = Self::edit_view(&view.borrow(), key, value);
            if let Some(before) = beside
                && let Err(error) = view.borrow().show(&before)
            {
                tracing::warn!(%error, "the item the window showed did not show again");
            }
            // The page is what the window showed, drawn again whole: the frame that takes the
            // item it showed and the one it showed the edit in is one.
            if let Some(shown) = self.lane_mut(lane) {
                shown.sent = None;
                shown.bound.clear();
            }
            outcome?;
            self.send_open(lane, None);
            return Ok(());
        }
        let view = place.into_view();
        Self::edit_view(&view.borrow(), key, value)?;
        if shown {
            self.shown_by_window(lane, key);
        }
        Ok(())
    }

    fn edit_view(view: &ViewInstance, key: &str, value: Value) -> Result<(), Words> {
        let value = match (&value, view.bound_value(key)) {
            (Value::Text(text), Some(Value::Int(_))) => Value::Int(
                text.trim()
                    .parse()
                    .map_err(|_| Words::Plain(String::new()))?,
            ),
            (Value::Text(text), Some(Value::Float(_))) => Value::Float(
                text.trim()
                    .parse()
                    .map_err(|_| Words::Plain(String::new()))?,
            ),
            _ => value,
        };
        match view.edit(key, &value) {
            Ok(EditOutcome::Applied) => Ok(()),
            Ok(EditOutcome::NeedsApproval) => Err(Words::phrase("tilolang-ask-needs-approval")),
            Ok(EditOutcome::Denied) => Err(Words::phrase("tilolang-ask-denied")),
            Err(error) => Err(Words::Plain(error.to_string())),
        }
    }

    fn resolve(&self, lane: Lane, key: &str, approve: bool) -> Result<(), Words> {
        let (Some(host), Some(view)) = (self.host(), self.view(lane)) else {
            return Ok(());
        };
        let target = view
            .borrow()
            .tree()
            .find(key)
            .and_then(|card| match card.props.get("invocation") {
                Some(Value::Ref(reference)) => Some(reference.target.clone()),
                _ => None,
            })
            .ok_or_else(|| Words::phrase("tilolang-open-failed"))?;
        let resolution = if approve {
            Resolution::Approve
        } else {
            Resolution::Decline
        };
        host.runtime()
            .resolve_invocation(&target, &Principal::new(self.me.clone()), resolution)
            .map(|_| ())
            .map_err(|error| Words::Plain(error.to_string()))
    }

    fn answer(&mut self, answer: ui::Answer) {
        let (Some(suggestion), Some(host)) = (self.ask.take(), self.host()) else {
            return;
        };
        let resolution = match answer {
            ui::Answer::Decline => Resolution::Decline,
            ui::Answer::Allow | ui::Answer::Always => {
                if answer == ui::Answer::Always {
                    for author in &suggestion.chain {
                        host.authority().trust(author);
                    }
                }
                Resolution::Approve
            }
        };
        if let Err(error) = host.runtime().resolve(
            &suggestion.run,
            &Principal::new(self.me.clone()),
            resolution,
        ) {
            self.notice(Words::Plain(error.to_string()), ui::NoticeKind::Error);
        }
        self.out.push(ToUi::Ask(None));
        self.commit_planned();
    }

    /// What follows an action a person ran from a control: a destructive one offers Undo once
    /// its commits (`tickets`) are in; what a primary one made is shown.
    fn after_action(
        &mut self,
        aref: &ActionRef,
        result: &Value,
        tickets: impl Iterator<Item = u64>,
    ) {
        let Some(host) = self.host() else {
            return;
        };
        let Some(action) = host.find_in_packages(|package| {
            package
                .meta
                .actions
                .iter()
                .find(|action| action.id == aref.action)
                .cloned()
        }) else {
            return;
        };
        match action.role.as_deref() {
            Some("destructive") => {
                let label = crate::runtime::platform::label_of(&action);
                // What it was run on: the item of a list (`delete(nb, n)`), or the resource.
                let subject = aref.args.iter().rev().find_map(|arg| match arg {
                    Value::Ref(subject) => Some(ResourceRef::clone(&subject.target)),
                    _ => None,
                });
                let title = subject
                    .map(|subject| title_of(&host, &subject))
                    .unwrap_or_default();
                let words = if title.is_empty() {
                    Words::Plain(label)
                } else {
                    Words::Phrase {
                        id: "tilolang-did".to_owned(),
                        args: vec![("action".to_owned(), label), ("title".to_owned(), title)],
                    }
                };
                if let Some(last) = tickets.max() {
                    self.pending.undo.insert(last, words);
                }
            }
            Some("primary") => {
                // What a primary action made opens beside the list, as the person asked for it.
                if let Value::Ref(made) = result {
                    self.pressed_opens
                        .push((self.last_fire, made.target.id_string()));
                }
            }
            _ => {}
        }
    }

    // ----- frames --------------------------------------------------------------------------

    /// The first half of a frame over the record's chunks: every chunk of the record this
    /// replica holds into the store, which reads and writes there the resources the record's
    /// manifest places in it, and reads the ones it was not sent when a read or a write reaches
    /// them ([`Host::sync_chunks`]); and the record's frame ([`Self::sync`]).
    pub fn sync_chunks(&mut self, chunks: &crate::chunk::Chunks) {
        if self.host.borrow().is_none() {
            // The host opens over the chunks this frame brings, and reads the rest of them as it
            // reaches into them.
            self.record_chunks = Some(chunks.clone());
        }
        let mut moved = false;
        if let Some(host) = self.host.borrow().as_ref().cloned() {
            moved |= host.sync_chunks(chunks);
        }
        // The record's frame reads the store the chunks were brought into, and opens the host
        // when it was not there yet: the chunks go in again after it, then.
        self.sync(chunks.first());
        if let Some(host) = self.host.borrow().as_ref().cloned() {
            moved |= host.sync_chunks(chunks);
        }
        if moved {
            self.moved = true;
        }
    }

    /// The first half of a frame: bring the runtime up to date with the workspace record `record`
    /// (`None` while the instance opens over the workspace's indexes), open the root once the
    /// indexes are read, stage a scenario, and send the window what changed.
    pub fn sync(&mut self, record: Option<&tilo_resource::Doc>) {
        let Some(host) = self.host_for(record) else {
            return;
        };
        let host = match record {
            Some(doc) => self.follow_updates(doc, host),
            None => host,
        };
        if !self.started {
            // Until the indexes hold rows to read, the lists would show nothing: the window
            // shows the platform's loading state. Once the first step of a build is taken in
            // it opens over those rows, and the rest of the record is read under them.
            if host.index_answers() {
                self.started = true;
                self.loading = false;
                if !self.rootless {
                    self.open_root(&host);
                }
            } else if !self.loading {
                self.loading = true;
                self.out.push(ToUi::Loading);
            }
        }
        // Only the origin runs the packages' triggers: every change is ordered there, and each
        // run records what it did, so a trigger runs once across the workspace's devices.
        if self.origin
            && self.mode == Mode::Live
            && let Some(now) = self.now()
            && host.run_triggers(now) > 0
        {
            self.moved = true;
        }
        if self.staged.is_some() {
            self.run_staged(&host);
        }
        if std::mem::take(&mut self.moved) {
            self.settle_due = true;
            if let Some(publication) = &self.publication {
                publication.update();
            }
            self.send_bound(Lane::Root);
            self.send_bound(Lane::Detail);
        } else if let Some(publication) = self.publication.as_ref().filter(|p| p.pending()) {
            publication.update();
        }
        self.poll_views();
        self.flush();
    }

    /// The record is the instance's now: it was opened over the indexes, and reads the record
    /// from here on.
    pub fn took_record(&mut self) {
        self.opening = false;
        self.record_loader = None;
    }

    /// Whether work is still under way off the frame that changes what the window shows: an
    /// index the store is deriving, or records the publication is reading. A window drawn while
    /// it is has not read the workspace the record holds.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.host().is_some_and(|host| host.index_pending())
            || self
                .publication
                .as_ref()
                .is_some_and(|publication| publication.pending())
    }

    /// Whether a frame has work the window has not seen: a change no sync has sent it yet
    /// (`moved`: the record moved, an index was taken in), a commit the door answered
    /// (`door_work`), an invocation the window handed the host (`outgoing`), a write the record's
    /// outbox owes ([`Self::settle_owed`]), or work under way off the frame
    /// ([`Self::pending`]). A frame taken while any of it holds shows what the workspace held
    /// before it, so a window that owes one syncs — and the host runs what it is owed — before it
    /// rests.
    #[must_use]
    pub fn owes_a_frame(&self) -> bool {
        self.moved
            || self.settle_owed()
            || self.door_work.get()
            || !self.outgoing.borrow().is_empty()
            || self.pending()
    }

    /// Whether the record's outbox owes a settlement a frame would take in: only a live window
    /// settles it, and only once it reads the record.
    fn settle_owed(&self) -> bool {
        self.settle_due && !self.opening && self.mode == Mode::Live
    }

    /// Wait for the work under way off the frame and take it in, so that what the window shows
    /// next is read from indexes and a publication that cover the record: for an embedder with
    /// no frame to take them in on. Whether what the window shows may have moved.
    pub fn settle_pending(&mut self) -> bool {
        let indexed = self.host().is_some_and(|host| host.wait_index());
        let published = self
            .publication
            .as_ref()
            .is_some_and(|publication| publication.settle());
        let moved = indexed || published;
        self.moved |= moved;
        moved
    }

    /// The second half of a frame: what the person did and what the door answered, run in the
    /// runtime, and what changed sent. Whether anything changed.
    pub fn update(&mut self) -> bool {
        let received = self.receive();
        let door = self.take_door_work();
        let opened = self.take_open_requests();
        let pulled = self.poll_origins();
        let settled = self.settle_outbox();
        // What the window calls a participant, or how the person writes a day or a time of day,
        // changed: the boundaries that show one run again, and no other.
        let named = std::mem::take(&mut self.names_changed);
        let formatted = std::mem::take(&mut self.formats_changed);
        if named || formatted {
            for view in [self.root.as_ref(), self.detail.as_ref()]
                .into_iter()
                .flatten()
            {
                let mut view = view.view.borrow_mut();
                if named {
                    view.names_moved();
                }
                if formatted {
                    view.formats_moved();
                }
            }
        }
        // Who is at a resource shows wherever a view reads its lenses.
        let lenses = std::mem::take(&mut self.lenses_changed);
        let members = self.follow_members();
        // The time a view read moved on: the frame its embedder woke for it.
        let timed = self.wakes_in().is_some_and(|wait| wait.is_zero());
        let changed = received
            || door
            || opened
            || pulled
            || settled
            || lenses
            || named
            || formatted
            || members
            || timed
            || self.views_stale();
        if changed {
            self.poll_views();
        }
        if named || formatted {
            // A name, and how a day or a time of day reads, go into a node's value when it is
            // sent: every tree is sent again.
            for lane in [Lane::Root, Lane::Detail] {
                self.resend(lane);
            }
        }
        // The presses ran and what they opened is sent: what the person typed after them can
        // go to the field focused now.
        self.tell_took();
        self.flush();
        changed
    }

    /// Give the runtime the workspace's members as the workspace's policy has them now, when it
    /// changed: who is in it, their roles, and who reads what. Whether it did.
    fn follow_members(&mut self) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let standing = self.standing.borrow().as_ref().and_then(|source| source());
        let Some(standing) = standing else {
            return false;
        };
        let seen = self
            .members_from
            .as_ref()
            .is_some_and(|(person, policy, people)| {
                *person == standing.person && *policy == *standing.policy && *people == self.people
            });
        if seen {
            return false;
        }
        host.runtime().set_members(
            crate::members::members_of(&standing, &self.people),
            Some(crate::members::reads_of(&standing, &self.people, &host)),
        );
        self.members_from = Some((
            standing.person,
            std::sync::Arc::unwrap_or_clone(standing.policy),
            self.people.clone(),
        ));
        true
    }

    /// Which person each participant presence reports is a device of, `(participant,
    /// person)`, where the workspace's host paired them: the policy's people are the runtime's
    /// principals by it.
    pub fn set_persons(&mut self, pairs: impl IntoIterator<Item = (String, String)>) {
        self.people = crate::members::People::new(&self.me, pairs);
    }

    /// Send the window the tree of `lane` again as patches, read anew.
    fn resend(&mut self, lane: Lane) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.lane_mut(lane) else {
            return;
        };
        let tree = window_tree(&host, &*names, &view.view);
        let mut patches = Vec::new();
        if let Some(sent) = &view.sent {
            wire::diff(sent, &tree, &mut patches);
        }
        view.sent = Some(tree);
        if !patches.is_empty() {
            self.out.push(ToUi::Patch { lane, patches });
        }
    }

    /// Send what waits as this frame's batch.
    pub fn flush(&mut self) {
        if self.out.is_empty() {
            return;
        }
        let messages = std::mem::take(&mut self.out);
        if messages
            .iter()
            .any(|message| matches!(message, ToUi::Patch { .. } | ToUi::RowsChanged { .. }))
        {
            self.patches_sent = self.patches_sent.saturating_add(1);
        }
        match ui::encode_batch(&messages) {
            Ok(batch) => {
                if let Err(error) = self.transport.send(batch) {
                    tracing::warn!(%error, "the window's connection failed");
                }
            }
            Err(error) => tracing::error!(%error, "a frame's batch did not encode"),
        }
    }
}

impl Drop for Instance {
    /// What the indexes hold reaches the disk as the instance ends: the next window of the workspace
    /// opens over them before its record.
    fn drop(&mut self) {
        if let Some(host) = self.host() {
            host.persist_index();
        }
    }
}

/// Why an action refused, as its form says it: what its `require` said, else the error.
fn refusal(error: &crate::runtime::RuntimeError) -> String {
    match error {
        crate::runtime::RuntimeError::Trap(why) | crate::runtime::RuntimeError::BadArgs(why) => {
            why.clone()
        }
        other => other.to_string(),
    }
}

/// `current` with `delete` bytes at `at` replaced by `insert`, each end moved back to the
/// nearest character boundary.
#[must_use]
pub fn spliced(current: &str, at: usize, delete: usize, insert: &str) -> String {
    let floor = |mut at: usize| {
        at = at.min(current.len());
        while !current.is_char_boundary(at) {
            at = at.saturating_sub(1);
        }
        at
    };
    let start = floor(at);
    let end = floor(at.saturating_add(delete)).max(start);
    let mut text = String::with_capacity(current.len().saturating_add(insert.len()));
    text.push_str(current.get(..start).unwrap_or_default());
    text.push_str(insert);
    text.push_str(current.get(end..).unwrap_or_default());
    text
}

/// The virtual lists of a view's tree, by key, with their keys from the root.
fn virtual_list_paths(tree: &UiNode) -> Vec<(String, Vec<String>)> {
    fn walk(node: &UiNode, path: &mut Vec<String>, out: &mut Vec<(String, Vec<String>)>) {
        path.push(node.key.clone());
        if ui::is_virtual_list(&node.kind) {
            out.push((node.key.clone(), path.clone()));
        }
        for child in &node.children {
            walk(child, path, out);
        }
        path.pop();
    }
    let mut out = Vec::new();
    walk(tree, &mut Vec::new(), &mut out);
    out
}

/// Render every row of the lists of `view` that no virtual list renders as it shows them: a
/// list inside content is drawn whole, one in a card as its preview (the rows the runtime built
/// for it).
fn show_content_rows(view: &mut ViewInstance) {
    fn lists(node: &UiNode, out: &mut Vec<String>) {
        if node.kind == "list" {
            out.push(node.key.clone());
        }
        for child in &node.children {
            lists(child, out);
        }
    }
    let mut shown = HashSet::new();
    loop {
        let mut found = Vec::new();
        lists(view.tree(), &mut found);
        found.retain(|key| !shown.contains(key));
        if found.is_empty() {
            return;
        }
        for list in found {
            for index in 0..view.list_len(&list).unwrap_or(0) {
                if let Err(error) = view.row(&list, index) {
                    tracing::warn!(%error, "a row of a list did not render");
                }
            }
            shown.insert(list);
        }
    }
}

/// What a view changed of a virtual list it renders as the list shows it.
#[derive(Default)]
struct ListChanges {
    /// Rows inserted, removed or moved, in order.
    structure: Vec<Patch>,
    /// The rows whose content changed.
    rows: Vec<String>,
    /// The list was replaced as a whole: its rows are read anew.
    reread: bool,
}

/// The patches that bring `sent`, the tree the window holds, to the view's `tree` where the
/// view's patches changed it (`changed`, paths of keys from the root), with `sent` brought there
/// too: each changed node converted alone, not the whole tree. `None`, and `sent` untouched,
/// when that cannot be done (a node not found, keys not unique): the tree is converted whole.
fn patch_in_place(
    sent: &mut ui::Node,
    tree: &UiNode,
    changed: &[Vec<String>],
    convert: &Convert<'_>,
) -> Option<Vec<ui::Patch>> {
    let mut paths: Vec<&[String]> = changed.iter().map(Vec::as_slice).collect();
    paths.sort_unstable();
    paths.dedup();
    // A change below one already converted is in it.
    let mut tops: Vec<&[String]> = Vec::with_capacity(paths.len());
    for path in paths {
        if !tops.iter().any(|top| path.starts_with(top)) {
            tops.push(path);
        }
    }
    let mut out = Vec::new();
    let mut fresh = Vec::with_capacity(tops.len());
    for path in tops {
        let (root, below) = path.split_first()?;
        if *root != tree.key || sent.key != tree.key {
            return None;
        }
        let node = below.iter().try_fold(tree, |node, key| {
            let mut named = node.children.iter().filter(|child| child.key == *key);
            let child = named.next()?;
            named.next().is_none().then_some(child)
        })?;
        let new = convert.node(node);
        if !wire::diff_at(wire::at(sent, below)?, &new, below, &mut out) {
            return None;
        }
        fresh.push((below, new));
    }
    for (below, new) in fresh {
        *wire::at_mut(sent, below)? = new;
    }
    Some(out)
}

/// What a view's patches changed, by where it shows.
#[derive(Default)]
struct Sorted {
    /// The virtual lists' rows, by list key.
    lists: BTreeMap<String, ListChanges>,
    /// Something besides those rows changed.
    content: bool,
    /// Where it changed, each a path of keys from the root; `None` when the root itself was
    /// replaced.
    changed: Option<Vec<Vec<String>>>,
}

impl Sorted {
    fn list(&mut self, key: &str) -> &mut ListChanges {
        self.lists.entry(key.to_owned()).or_default()
    }
}

/// Sort a view's `patches` against its virtual lists `lists` (each with its keys from the root):
/// rows inserted, removed or moved, and rows changed, are their lists'; the rest is content.
fn sort(patches: Vec<Patch>, lists: &[(String, Vec<String>)]) -> Sorted {
    let mut sorted = Sorted {
        changed: Some(Vec::new()),
        ..Sorted::default()
    };
    for patch in patches {
        let (path, structural) = match &patch {
            Patch::SetRoot(_) => {
                sorted.content = true;
                sorted.changed = None;
                for (list, _) in lists {
                    sorted.list(list).reread = true;
                }
                continue;
            }
            // What a list holds beside its rows — the slots a host fills with rows of its own,
            // such as the items someone removed that the person has not seen go — is no row of
            // the list: its rows are the ones the steps name, and the slot stands as content.
            Patch::Insert { parent, node, .. } if node.kind == wire::SLOT => {
                (parent.as_slice(), false)
            }
            // A row the steps take out says where it stood; one they do not is no row.
            Patch::Remove {
                parent, at: None, ..
            } => (parent.as_slice(), false),
            Patch::Insert { parent, .. }
            | Patch::Remove { parent, .. }
            | Patch::Move { parent, .. } => (parent.as_slice(), true),
            Patch::SetProp { path, .. }
            | Patch::SetBinding { path, .. }
            | Patch::SetEvent { path, .. }
            | Patch::SetAnchor { path, .. } => (path.as_slice(), false),
        };
        // The innermost list the patch is of: a list inside a row — the document of a row
        // standing open can hold one — takes the changes of its own rows, and the list of that
        // row the rest of the row.
        let of_list = lists
            .iter()
            .filter(|(_, at)| {
                (structural && path == at.as_slice())
                    || (path.len() > at.len() && path.starts_with(at))
            })
            .max_by_key(|(_, at)| at.len());
        // What holds a list changed: the list may be another now. A row inserted, removed or
        // moved carries the lists inside it along.
        if structural && of_list.is_none_or(|(_, at)| path != at.as_slice()) {
            for (list, at) in lists {
                if at.len() > path.len() && at.starts_with(path) {
                    sorted.list(list).reread = true;
                }
            }
        }
        match of_list {
            Some((list, at)) if path == at.as_slice() => {
                sorted.list(list).structure.push(patch.clone());
            }
            Some((list, at)) => {
                if let Some(row) = path.get(at.len()) {
                    let changes = sorted.list(list);
                    if !changes.rows.contains(row) {
                        changes.rows.push(row.clone());
                    }
                }
            }
            None => {}
        }
        if of_list.is_none() {
            sorted.content = true;
            if let Some(changed) = &mut sorted.changed {
                changed.push(path.to_vec());
            }
        }
    }
    sorted
}

/// The other packages of the workspace whose records the package `app` holds references to.
fn used_packages(host: &Host, app: &str) -> Vec<Package> {
    let Some(own) = host.package(app) else {
        return Vec::new();
    };
    host.packages()
        .into_iter()
        .filter(|other| other.meta.app != app)
        .filter(|other| {
            if own.meta.uses.contains(&other.meta.app) {
                return true;
            }
            let prefix = format!("{}.", other.meta.app);
            own.meta
                .types
                .iter()
                .flat_map(|ty| &ty.fields)
                .any(|field| field.ty.contains(&prefix))
        })
        .collect()
}

/// The package that declares the type `ty` (an id), with the type.
#[must_use]
pub fn type_of(host: &Host, ty: &str) -> Option<(Meta, TypeMeta)> {
    host.find_in_packages(|package| {
        let declared = package
            .meta
            .types
            .iter()
            .find(|declared| declared.id == ty)?
            .clone();
        Some((package.meta.clone(), declared))
    })
}

/// The name a bound control is announced by, as `field` says what a field of a resource is
/// called. A view state's name is a name in the code (`draft`, `step`), not a word a person
/// says: a control bound to one is named by what the view says it is for — its placeholder,
/// its label — and carries no name of ours.
#[must_use]
pub fn announced(binding: &Binding, field: impl FnOnce(&ResourceRef, &str) -> String) -> String {
    match &binding.target {
        BindTarget::State { .. } => String::new(),
        BindTarget::Field {
            target,
            field: name,
        } => field(target, name),
    }
}

/// `value` with every reference in it resolved through `resolve`, which names the type a
/// reference that arrived by its id alone belongs to: the resource a drop on a view carried,
/// at every place a value holds one (a list a drop carries `[resource, at]`, a tuple). A
/// reference already naming its type, and one `resolve` does not know, is left as it is, so a
/// value never loses what it reached here with.
fn resolved_value(value: Value, resolve: &dyn Fn(&ResourceRef) -> Option<ResourceRef>) -> Value {
    match value {
        Value::Ref(held) => {
            // Only a reference that came by its id alone is read for its type: one that names it
            // is what the view gave (and keeps its shared target), and one `resolve` does not
            // know stays as it arrived, so what the event carried is never lost.
            let Some(target) = held
                .target
                .ty
                .is_empty()
                .then(|| resolve(&held.target))
                .flatten()
            else {
                return Value::Ref(held);
            };
            Value::Ref(RefValue {
                target: Rc::new(target),
                author: held.author,
            })
        }
        Value::List(items) => Value::List(
            items
                .into_iter()
                .map(|item| resolved_value(item, resolve))
                .collect(),
        ),
        Value::Tuple(items) => Value::Tuple(
            items
                .into_iter()
                .map(|item| resolved_value(item, resolve))
                .collect(),
        ),
        Value::Opt(Some(inner)) => Value::Opt(Some(Box::new(resolved_value(*inner, resolve)))),
        other => other,
    }
}

/// A name in sentence case: `new_note` and `NewNote` read "New note".
#[must_use]
pub fn sentence(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for ch in name.chars() {
        if ch == '_' || ch == ' ' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if ch.is_uppercase() && !word.is_empty() {
            words.push(std::mem::take(&mut word));
            word.extend(ch.to_lowercase());
        } else {
            word.extend(ch.to_lowercase());
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let joined = words.join(" ");
    let mut chars = joined.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The title of a resource, as every surface that names it shows it: its first text field's
/// first line; else, for a package's root, the package's title; else what its type declares for
/// it while untitled; else its type's name.
#[must_use]
pub fn title_of(host: &Host, resource: &ResourceRef) -> String {
    host.runtime().title(resource).unwrap_or_default()
}

/// The cases of the enum `id`: each case's name, what a control shows for it, and its icon's
/// name.
#[must_use]
pub fn cases(host: &Host, id: &str) -> Vec<wire::Case> {
    host.find_in_packages(|package| {
        package
            .meta
            .enums
            .iter()
            .find(|declared| declared.id == id || declared.name == id)
            .map(|declared| {
                declared
                    .variants
                    .iter()
                    .map(|variant| {
                        let label = if variant.label.is_empty() {
                            sentence(&variant.name)
                        } else {
                            variant.label.clone()
                        };
                        wire::Case {
                            name: variant.name.clone(),
                            label,
                            icon: variant.icon.as_ref().map(|icon| icon.replace('_', "-")),
                        }
                    })
                    .collect()
            })
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_in_sentence_case() {
        assert_eq!(sentence("new_note"), "New note");
        assert_eq!(sentence("NewNote"), "New note");
        assert_eq!(sentence("toggle_pin"), "Toggle pin");
        assert_eq!(sentence("Note"), "Note");
        assert_eq!(sentence(""), "");
    }

    /// A control bound to view state is not announced by that state's name in the code, which
    /// a person never says; it is announced by what the view says it is for.
    #[test]
    fn a_bound_control_is_not_named_by_a_name_in_the_code() {
        let state = Binding {
            target: BindTarget::State {
                key: crate::runtime::StateKey::view("w", "person:me", "draft"),
                name: "draft".to_owned(),
                ty: "Text".to_owned(),
            },
            author: crate::runtime::ModuleHash(String::new()),
        };
        assert_eq!(
            announced(&state, |_, _| "should not be asked".to_owned()),
            ""
        );
        let field = Binding {
            target: BindTarget::Field {
                target: ResourceRef::parse_id("r1", String::new()).expect("a resource id"),
                field: "due".to_owned(),
            },
            author: crate::runtime::ModuleHash(String::new()),
        };
        assert_eq!(announced(&field, |_, field| sentence(field)), "Due");
    }

    #[test]
    fn a_splice_lands_on_character_boundaries() {
        assert_eq!(spliced("Q3 plan", 7, 0, "s"), "Q3 plans");
        assert_eq!(spliced("Q3 plans", 2, 6, ""), "Q3");
        assert_eq!(
            spliced("naïve", 3, 1, "i"),
            "nai\u{308}ve".replace("i\u{308}", "i")
        );
        assert_eq!(spliced("", 9, 9, "x"), "x");
    }
}
