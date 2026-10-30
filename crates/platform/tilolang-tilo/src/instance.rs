//! An app instance: one window of a tilolang app as its app host runs it, without the toolkit.
//!
//! The instance holds what the window shows and does, as data: the host over the room record
//! (or its indexes, before the record), the window's two views (the root, and the view of the
//! resource `open(r)` or a row went to), the rows of their virtual lists, what bound controls
//! show, and the commits waiting for the door. It sends its window component patches and hears
//! what the person did, over the patch protocol (`tilo_render_protocol::patch`), through any
//! transport: a queue beside a window in one process, the UI host's socket, or `postMessage`
//! from a worker. What builds, lays out, draws and takes input is the window's.
//!
//! Each frame the instance
//!
//! 1. brings the runtime's mirror of the room record up to date with the record,
//! 2. takes what the person did and runs it in the runtime, which plans the change,
//! 3. re-runs a view if what it read changed, and sends the window the patches that bring its
//!    copy of the view's tree up to date, and
//! 4. queues what the runtime planned for the door, as an invocation of the host's `commit`
//!    action, so the change is attributed, undoable and replayed by every peer.
//!
//! What holds for every window is the platform's, and so the instance's: the lenses on each
//! resource are what presence reports; a destructive action a person runs offers Undo; a
//! primary action a person runs shows what it made.

pub mod form;
#[cfg(feature = "room")]
pub mod room;
pub mod rows;
pub mod wire;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value as Json;
use tilo_actions::{ActionAddress, ControlInvocation, InvocationId};
use tilo_render_protocol::patch::{self as ui, Lane, ToApp, ToUi, Transport, Words};

use crate::background::Waker;
use crate::bundle::Bundle;
use crate::host::{Host, Opening, Package};
use crate::publish::{COMMIT, HOST_RESOURCE};
use crate::runtime::meta::{Meta, TypeMeta};
use crate::runtime::{
    ActionRef, BindTarget, Binding, EditOutcome, InvokeOutcome, NameHost, OutsideSink, Patch,
    Principal, Resolution, ResourceRef, Suggestion, UiNode, Value, ViewEvent, ViewInstance,
};
use crate::upgrade::Upgrade;
use rows::{FIRST_ROWS, ListRows};
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

/// What the person asked of their history at the window: the record's undo or redo, as the one
/// who runs the instance carries it out on the record (`ToApp::Undo`, `ToApp::Redo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryAsk {
    /// Take back their last step.
    Undo,
    /// Bring back what their last undo took back.
    Redo,
}

/// A later build of the package, in a development session.
#[derive(Clone)]
pub struct Update {
    /// Counts the builds taken since launch, from 1.
    pub generation: u64,
    pub bundle: Arc<Bundle>,
}

/// What the instance publishes on the door while it lives: the room's records with their
/// actions, kept up to date change by change.
pub trait Publication {
    /// Bring what is published up to date with the record.
    fn update(&self);
    /// Whether records are still being read off the frame.
    fn pending(&self) -> bool;
}

/// What an instance asks of whatever runs it.
pub trait Platform {
    /// Load what a window of the package runs into `host`: the installed package, or the latest
    /// later build of it, with the packages it uses. The later build it
    /// loaded, 0 for the installed package.
    fn load(&self, host: &Host) -> u64;

    /// The latest later build of the package, once a development session handed one over.
    fn latest(&self) -> Option<Update> {
        None
    }

    /// Wake this instance when a later build arrives. Called once, when it first follows them.
    fn follow_updates(&self) {}

    /// Whether this process follows later builds at all.
    fn follows_updates(&self) -> bool {
        false
    }

    /// A later build took over: what the package declares on the door is its now.
    fn surface_changed(&self, app: &str, bundle: &Bundle) {
        let _ = (app, bundle);
    }

    /// The platform's phrase `id` with `args`, in the person's language: what the standard
    /// library's frames say.
    fn phrase(&self, id: &str, args: &[(String, String)]) -> String;

    /// The time now, as `now` reads it: ISO-8601 UTC text.
    fn now(&self) -> String;

    /// How many seconds the person's clock is ahead of UTC: the zone `dated` reads "tomorrow
    /// 9am" in. UTC unless the platform knows the person's.
    fn zone(&self) -> i64 {
        0
    }

    /// What wakes the instance when work it waits for is ready (an index read off the frame).
    fn waker(&self) -> Waker;

    /// Publish what `host` holds until the returned hold is dropped; `wake` is called when
    /// records read off the frame are ready to be taken in.
    fn publish(&self, host: &Host, wake: Waker) -> Option<Box<dyn Publication>> {
        let _ = (host, wake);
        None
    }
}

/// How an instance starts.
pub struct Config {
    /// The package the window runs.
    pub app: String,
    /// The participant at the window.
    pub me: String,
    pub mode: Mode,
    /// The device that carries out the outside effects the room queues.
    pub device: Option<String>,
    /// Where the indexes of the room are kept; `None` keeps them in memory.
    pub index_dir: Option<std::path::PathBuf>,
    /// Reads the room record when the instance opens over the indexes before it: a write, or a
    /// field no index holds, waits for it.
    pub record_loader: Option<Box<dyn Fn() -> Option<tilo_resource::Doc>>>,
    /// Where outside effects go; the development log when `None`.
    pub sink: Option<Box<dyn OutsideSink>>,
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

/// The names of the room, as the runtime asks for them.
#[derive(Clone)]
struct SharedNames(Rc<dyn NameHost>);

impl NameHost for SharedNames {
    fn name(&self, who: &Principal) -> String {
        self.0.name(who)
    }
}

/// One of the window's two views: a resource shown through a view.
struct Shown {
    target: ResourceRef,
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
    me: String,
    mode: Mode,
    device: Option<String>,
    index_dir: Option<std::path::PathBuf>,
    record_loader: Option<Box<dyn Fn() -> Option<tilo_resource::Doc>>>,
    /// Where the host reads the person's standing in the room, once its embedder has one.
    standing: Rc<RefCell<Option<Rc<crate::authority::StandingSource>>>>,
    /// Whether the instance shows the room before its record, over its indexes.
    opening: bool,
    platform: Rc<dyn Platform>,
    names: Rc<dyn NameHost>,
    /// What the window said it calls people, which `names` reads first.
    told: Rc<crate::names::Told>,
    host: Rc<RefCell<Option<Host>>>,
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
    lenses: HashMap<String, Vec<(Principal, bool)>>,
    lenses_changed: bool,
    /// The person and the room's policy the runtime's members were last read from.
    members_from: Option<(
        String,
        tilo_action_policy::RoomPolicy,
        crate::members::People,
    )>,
    /// How the policy's people are the runtime's principals, as presence pairs them.
    people: crate::members::People,
    names_changed: bool,
    outgoing: Outgoing,
    in_flight: VecDeque<u64>,
    door_work: Rc<Cell<bool>>,
    open_requests: Rc<RefCell<Vec<String>>>,
    /// What a person's press of a primary action made, to open as they asked for it.
    pressed_opens: Vec<String>,
    ask: Option<Suggestion>,
    settle_due: bool,
    sink: Option<Box<dyn OutsideSink>>,
    /// The outside services the package's types with an origin are kept in step with.
    origins: crate::origin::Origins,
    pending: Pending,
    /// The undos and redos the person asked for, in order, not yet taken.
    history: Vec<HistoryAsk>,
    /// The scenario a capture stages, until the host runs it.
    staged: Option<String>,
    started: bool,
    loading: bool,
    moved: bool,
    /// Whether the package's migrations ran over the room record since the window opened.
    migrated: bool,
    /// Whether this replica is the room's origin, the one place the package's migrations run
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
    /// How many presses the window sent (`ToApp::Fire`) the instance ran, and how many of
    /// them it told the window it ran (`ToUi::Took`).
    fired: u64,
    fired_told: u64,
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
            outgoing: Rc::default(),
            in_flight: VecDeque::new(),
            door_work: Rc::default(),
            open_requests: Rc::default(),
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
            staged: None,
            started: false,
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
            fired_told: 0,
            form: None,
            forms: 0,
        }
    }

    /// This instance on the device `device`: the one that carries out the outside effects its
    /// room queues. Before the host starts.
    pub fn set_device(&mut self, device: Option<String>) {
        self.device = device;
    }

    /// Keep the room's indexes in `dir` (`None` keeps them in memory). Before the host starts.
    pub fn set_index_dir(&mut self, dir: Option<std::path::PathBuf>) {
        self.index_dir = dir;
    }

    /// Open over the room's indexes before its record, which `loader` reads: a write, or a
    /// field no index holds, waits for it. Before the host starts.
    pub fn set_record_loader(&mut self, loader: Box<dyn Fn() -> Option<tilo_resource::Doc>>) {
        self.opening = true;
        self.record_loader = Some(loader);
    }

    /// Read the person's standing in the room from `source`: what the runtime invokes is decided
    /// by the room's policy with it. May come after the host started (a window shown before its
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

    /// The anchors search hits and agents asked the window to show, until it shows them.
    #[must_use]
    pub fn open_requests(&self) -> Rc<RefCell<Vec<String>>> {
        Rc::clone(&self.open_requests)
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

    /// The bound control the window's keyboard focus is in, and whether an input method is
    /// composing in it.
    #[must_use]
    pub fn focus(&self) -> (Option<&(Lane, String)>, bool) {
        (self.focus.as_ref(), self.composing)
    }

    /// Run the scenario `scenario` once the host runs, as a capture stages it.
    pub fn stage(&mut self, scenario: Option<&str>) {
        self.staged = scenario.map(str::to_owned);
    }

    /// The room record moved: what the views read may have changed, and outside effects may
    /// wait.
    pub fn moved(&mut self) {
        self.moved = true;
    }

    /// The names of the room changed: views showing names show them again.
    pub fn names_changed(&mut self) {
        self.names_changed = true;
    }

    /// Say whether this replica is the room's origin (the device that governs the room). Only
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

    /// The host over the room record `doc`, built with the package the first time; without the
    /// record (while it is read off the frame), over the room's indexes.
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
        let platform = Rc::clone(&self.platform);
        host.set_clock(move || platform.now());
        let platform = Rc::clone(&self.platform);
        host.runtime().set_zone(move || platform.zone());
        let platform = Rc::clone(&self.platform);
        host.set_phrases(move |id, args| platform.phrase(id, args));
        if let Some(sink) = self.sink.take() {
            host.runtime().set_sink(crate::origin::RoutingSink {
                origins: Rc::clone(&self.origins),
                rest: sink,
            });
        }
        self.generation = self.platform.load(&host);
        if self.platform.follows_updates() && self.mode != Mode::Past && !self.follows {
            self.follows = true;
            self.platform.follow_updates();
        }
        if self.mode != Mode::Past {
            // What is published and found of the room is read off the frame the first time;
            // the instance takes it in when it is ready.
            host.set_index_waker(self.platform.waker());
            self.publication = self.platform.publish(&host, self.platform.waker());
        }
        *self.host.borrow_mut() = Some(host.clone());
        Some(host)
    }

    /// The name a bound control is announced by: the field or view state it edits.
    fn field_name(host: &Host, binding: &Binding) -> String {
        let (target, field) = match &binding.target {
            BindTarget::State { name, .. } => return sentence(name),
            BindTarget::Field { target, field } => (target, field),
        };
        type_of(host, &target.ty)
            .and_then(|(_, ty)| {
                ty.fields
                    .iter()
                    .find(|declared| declared.id == *field)
                    .map(|declared| sentence(&declared.name))
            })
            .unwrap_or_default()
    }

    // ----- later builds --------------------------------------------------------------------

    /// Take over the latest later build of the package, when this instance runs an older one:
    /// in place when it reads the state the old one wrote, and otherwise by starting over on
    /// the room record `doc` with it, as the app's next start would. The host the instance runs
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
        let (Ok(target), Ok(view)) = (host.root_of(&self.app), host.open_main(&self.app)) else {
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

    /// Start over on the room record `doc`: a new runtime that loads the latest build and reads
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

    // ----- the views -----------------------------------------------------------------------

    /// Open the package's root view.
    fn open_root(&mut self, host: &Host) {
        let target = match host.root_of(&self.app) {
            Ok(target) => target,
            Err(error) => {
                tracing::warn!(app = self.app, %error, "the app has no root to open");
                return;
            }
        };
        match host.open_main(&self.app) {
            Ok(view) => {
                self.root = Some(Shown::new(target, self.spaced(Lane::Root, view)));
                if self.detail.take().is_some() {
                    self.out.push(ToUi::Close);
                }
                self.send_open(Lane::Root, None);
                self.commit_planned();
            }
            Err(error) => {
                tracing::warn!(app = self.app, %error, "the app did not open");
                self.notice(Words::phrase("tilolang-open-failed"), ui::NoticeKind::Error);
            }
        }
    }

    /// Show `target` in the detail view: what `open(r)` asked for, or a row. With `front`, a
    /// narrow window brings it to the front.
    /// Open what the reference `reference` (a `mention` mark's value) in rich text refers to:
    /// a resource of a package here that the viewer holds, in the detail lane.
    fn open_reference(&mut self, reference: &str) {
        use crate::runtime::Store as _;
        let Some(reference) = crate::runtime::rich::Reference::of_mark(reference) else {
            return;
        };
        let Some(host) = self.host() else {
            return;
        };
        let Some(mut target) = referenced_resource(&host, &reference) else {
            return;
        };
        let Ok(ty) = host.store().type_of(&target) else {
            return;
        };
        target.ty = ty;
        self.open_detail(&target, None, Some(ui::Front::Input));
    }

    fn open_detail(&mut self, target: &ResourceRef, with: Option<&str>, front: Option<ui::Front>) {
        let Some(host) = self.host() else {
            return;
        };
        if self
            .detail
            .as_ref()
            .is_some_and(|detail| detail.target == *target && with.is_none())
        {
            if let Some(front) = front {
                self.out.push(ToUi::Front(front));
            }
            return;
        }
        match host.open_resource(target, with) {
            Ok(view) => {
                let mut view = Shown::new(target.clone(), self.spaced(Lane::Detail, view));
                view.with = with.map(str::to_owned);
                self.detail = Some(view);
                self.lenses_changed = true;
                self.send_open(Lane::Detail, front);
                self.commit_planned();
            }
            Err(error) => {
                tracing::warn!(%error, "a resource did not open");
                self.notice(Words::phrase("tilolang-open-failed"), ui::NoticeKind::Error);
            }
        }
    }

    /// Close the detail view: Back.
    pub fn close_detail(&mut self) {
        if self.detail.take().is_some() {
            self.lenses_changed = true;
            self.out.push(ToUi::Close);
        }
    }

    /// Send the window the view of `lane` as it is now: its tree, and the first rows of its
    /// virtual lists.
    fn send_open(&mut self, lane: Lane, front: Option<ui::Front>) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.lane_mut(lane) else {
            return;
        };
        view.lists.clear();
        view.bound.clear();
        let tree = {
            let instance = view.view.borrow();
            convert_with(&host, &*names, &instance, |convert| {
                convert.node(instance.tree())
            })
        };
        view.sent = Some(tree.clone());
        let target = wire::resource(&view.target);
        self.out.push(ToUi::Open {
            lane,
            target,
            tree,
            front,
        });
        self.track_lists(lane);
        self.send_bound(lane);
    }

    /// Keep the rows of every virtual list of the view of `lane`, sending the window the first
    /// rows of each new one, and drop those of lists gone.
    fn track_lists(&mut self, lane: Lane) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.lane_mut(lane) else {
            return;
        };
        let lists = virtual_lists(view.view.borrow().tree());
        let handle = Rc::clone(&view.view);
        view.lists
            .retain(|key, rows| lists.contains(key) && rows.reads(&handle, key));
        let mut sent = Vec::new();
        for list in lists {
            if view.lists.contains_key(&list) {
                continue;
            }
            let mut rows = ListRows::of(&handle, &list);
            let first = 0..FIRST_ROWS.min(rows.len());
            let requested = rows.request(first);
            sent.push(rows_message(
                &host, &*names, &handle, lane, &list, requested,
            ));
            view.lists.insert(list, rows);
        }
        self.out.extend(sent);
    }

    /// Re-run the views whose reads changed, and send the window what changed: their trees'
    /// patches, and the changes of their virtual lists' rows. Whether anything did.
    fn poll_views(&mut self) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let names = Rc::clone(&self.names);
        let mut changed = false;
        for lane in [Lane::Root, Lane::Detail] {
            let Some(view) = self.lane_mut(lane) else {
                continue;
            };
            let patches = match view.view.borrow_mut().poll_patches() {
                Ok(patches) => patches,
                Err(error) => {
                    tracing::warn!(%error, "a view failed to run");
                    continue;
                }
            };
            if patches.is_empty() {
                continue;
            }
            changed = true;
            let lists = virtual_list_paths(view.view.borrow().tree());
            let sorted = sort(patches, &lists);
            let mut messages = Vec::new();
            let handle = Rc::clone(&view.view);
            for (list, changes) in sorted.lists {
                let Some(rows) = view.lists.get_mut(&list) else {
                    continue;
                };
                let row_changes = if changes.reread {
                    rows.reread()
                } else {
                    let mut structural = rows.apply(&changes.structure);
                    let changed = rows.changed(&changes.rows);
                    let instance = handle.borrow();
                    convert_with(&host, &*names, &instance, |convert| {
                        structural.extend(changed.iter().map(|row| ui::RowChange::Changed {
                            row: convert.node(row),
                        }));
                    });
                    structural
                };
                if row_changes.is_empty() {
                    continue;
                }
                let len = u32::try_from(rows.len()).unwrap_or(u32::MAX);
                messages.push(ToUi::RowsChanged {
                    lane,
                    list,
                    len,
                    changes: row_changes,
                });
            }
            if sorted.content {
                // A list inside the content may have rows the view has not rendered yet.
                show_content_rows(&mut view.view.borrow_mut());
                let tree = {
                    let instance = view.view.borrow();
                    convert_with(&host, &*names, &instance, |convert| {
                        convert.node(instance.tree())
                    })
                };
                let mut patches = Vec::new();
                match &view.sent {
                    Some(sent) => wire::diff(sent, &tree, &mut patches),
                    None => patches.push(ui::Patch::SetRoot(tree.clone())),
                }
                view.sent = Some(tree);
                if !patches.is_empty() {
                    self.out.push(ToUi::Patch { lane, patches });
                }
            }
            self.out.extend(messages);
            if sorted.content {
                self.track_lists(lane);
            }
        }
        if changed {
            self.send_bound(Lane::Root);
            self.send_bound(Lane::Detail);
        }
        changed
    }

    /// The opens the views asked for.
    fn follow_opens(&mut self) {
        let mut opens = Vec::new();
        for view in [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
        {
            opens.extend(view.view.borrow_mut().poll_events());
        }
        for event in opens {
            match event {
                ViewEvent::Open {
                    target,
                    view,
                    front,
                } => {
                    // A selection the keyboard moves shows the item beside the list without
                    // bringing it to the front of a narrow window; an activation does.
                    let front = front.then_some(ui::Front::Input);
                    self.open_detail(&target, view.as_deref(), front);
                }
            }
        }
    }

    /// Send the window what each bound control of the view of `lane` (and of the rows it
    /// shows) holds now, where that changed.
    fn send_bound(&mut self, lane: Lane) {
        let Some(view) = self.lane_mut(lane) else {
            return;
        };
        let instance = Rc::clone(&view.view);
        let instance = instance.borrow();
        let mut now: Vec<(String, ui::Value)> = Vec::new();
        bound_values(instance.tree(), &instance, &mut now);
        for rows in view.lists.values() {
            for key in rows.shown() {
                if let Some(row) = instance.find(key) {
                    bound_values(row, &instance, &mut now);
                }
            }
        }
        let mut sent = Vec::new();
        for (key, value) in now {
            if view.bound.get(&key) == Some(&value) {
                continue;
            }
            view.bound.insert(key.clone(), value.clone());
            let acked = view.acked.get(&key).copied().unwrap_or(0);
            sent.push(ToUi::Bound {
                lane,
                key,
                value,
                acked,
            });
        }
        drop(instance);
        self.out.extend(sent);
    }

    /// The window shows what was typed into its bound control `key` of `lane` (the edit it
    /// sent just applied): take it as what the window holds, so a later change from elsewhere
    /// (the draft cleared once it was sent) differs from it and is sent.
    fn shown_by_window(&mut self, lane: Lane, key: &str) {
        let Some(value) = self
            .view(lane)
            .and_then(|view| view.borrow().bound_value(key))
        else {
            return;
        };
        let shown = plain(&NoNames).value(&value);
        if let Some(view) = self.lane_mut(lane) {
            view.bound.insert(key.to_owned(), shown);
        }
    }

    /// Bring the lenses the views read up to date with presence: who else is at each resource
    /// (`peers`, by resource id), and the person themselves at the detail view's resource.
    pub fn set_peers(&mut self, peers: HashMap<String, String>) {
        let mut now: HashMap<String, Vec<(Principal, bool)>> = HashMap::new();
        for (resource, who) in peers {
            now.entry(resource)
                .or_default()
                .push((Principal::new(who), false));
        }
        if let Some(detail) = &self.detail {
            now.entry(detail.target.id_string())
                .or_default()
                .push((Principal::new(self.me.clone()), true));
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

    /// Run the scenario a capture stages: its hidden action fills the room.
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
            tracing::warn!(app = self.app, scenario = name, "no such scenario");
            return;
        };
        // The packages this one points into (a board's apps) are staged first, each with its
        // first scenario, so a scenario that attaches their roots shows them filled.
        for used in used_packages(host, &self.app) {
            if let Some(first) = used.meta.scenarios.first() {
                let _ = self.run_scenario(host, &used.meta.app, &first.name, &first.action);
            }
        }
        let shown = self.run_scenario(host, &self.app.clone(), &name, &scenario.action);
        self.commit_planned();
        // A scenario that returns a resource shows it, as a primary action shows what it made.
        // A script opened it, so a narrow window shows it in front at once.
        if let Some(shown) = shown {
            self.open_detail(&shown, None, Some(ui::Front::Action));
        }
    }

    /// Run the scenario `name` (its hidden action `action`) of the package `app` over its root;
    /// the resource it returned, when it returns one.
    fn run_scenario(
        &self,
        host: &Host,
        app: &str,
        name: &str,
        action: &str,
    ) -> Option<ResourceRef> {
        if let Err(error) = host.root_of(app) {
            tracing::warn!(app, %error, "the package has no root to stage");
            return None;
        }
        match host
            .runtime()
            .invoke_action(&Principal::new(self.me.clone()), (action, Vec::new()))
        {
            Ok(InvokeOutcome::Done(Value::Ref(shown))) => {
                return Some(Rc::unwrap_or_clone(shown.target));
            }
            Ok(InvokeOutcome::Done(_)) => {}
            Ok(other) => tracing::warn!(scenario = name, ?other, "the scenario did not run"),
            Err(error) => tracing::warn!(scenario = name, %error, "the scenario failed"),
        }
        None
    }

    // ----- the door ------------------------------------------------------------------------

    fn notice(&mut self, words: Words, kind: ui::NoticeKind) {
        self.out.push(ToUi::Notice {
            words,
            kind,
            undo: false,
        });
    }

    /// The record's changes stay on this device, for `why` (the room refused them for good):
    /// the window says so.
    pub fn unshared(&mut self, why: String) {
        self.notice(Words::Plain(why), ui::NoticeKind::Error);
        self.flush();
    }

    /// The undo or redo the person asked for was refused, for `why` (somebody else changed what
    /// it would take back since): the window says so.
    pub fn history_refused(&mut self, why: String) {
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
                host.packages().iter().find_map(|package| {
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
        let anchors: Vec<(String, ui::Front)> = std::mem::take(&mut self.pressed_opens)
            .into_iter()
            .map(|anchor| (anchor, ui::Front::Input))
            .chain(
                std::mem::take(&mut *self.open_requests.borrow_mut())
                    .into_iter()
                    .map(|anchor| (anchor, ui::Front::Action)),
            )
            .collect();
        if anchors.is_empty() {
            return false;
        }
        let Some(host) = self.host() else {
            return false;
        };
        for (anchor, front) in anchors {
            // A hit names the published resource (`tl:<app>:<id>`) or the resource's id.
            let anchor = anchor
                .strip_prefix("tl:")
                .and_then(|rest| rest.split_once(':'))
                .map_or(anchor.as_str(), |(_, id)| id)
                .to_owned();
            match host.open_plan(&anchor) {
                Ok(plan) => {
                    if let Some(target) = plan.target
                        && (target.item.is_some() || plan.app != self.app)
                    {
                        self.open_detail(&target, None, Some(front));
                    }
                    self.commit_planned();
                }
                Err(error) => {
                    tracing::warn!(%error, "a search hit named nothing in the room");
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
            Ok(0) => false,
            Ok(_) => {
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
            ToApp::Undo => self.history.push(HistoryAsk::Undo),
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

    fn event(&mut self, message: ToApp) {
        let first = self.host().map(|host| host.next_ticket());
        let mut ran: Option<(ActionRef, Value)> = None;
        let pressed = matches!(message, ToApp::Fire { .. });
        let outcome = match message {
            ToApp::Back => {
                self.close_detail();
                Ok(())
            }
            ToApp::Fire {
                lane,
                key,
                event,
                value,
            } => self.fire(lane, &key, &event, value.as_ref(), &mut ran),
            edit @ (ToApp::Toggle { .. }
            | ToApp::Splice { .. }
            | ToApp::Edit { .. }
            | ToApp::EditRich { .. }) => self.edit_event(edit),
            ToApp::Resolve { lane, key, approve } => self.resolve(lane, &key, approve),
            ToApp::Open { resource } => {
                if let Some(target) = wire::runtime_resource(&resource) {
                    self.open_detail(&target, None, Some(ui::Front::Input));
                }
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
            } => {
                self.request_rows(lane, &list, start as usize..end as usize);
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
            ToApp::FormSubmit { form } => self.form_submit(form, &mut ran),
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
        };
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
            self.take_open_requests();
            self.tell_took();
        }
    }

    /// Tell the window how many of its presses ran, when that changed: what they opened was
    /// sent before.
    fn tell_took(&mut self) {
        if self.fired != self.fired_told {
            self.fired_told = self.fired;
            self.out.push(ToUi::Took { fired: self.fired });
        }
    }

    /// Render the rows `range` of the list `list` of the view of `lane`, and send them.
    fn request_rows(&mut self, lane: Lane, list: &str, range: std::ops::Range<usize>) {
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
        let requested = rows.request(range);
        let message = rows_message(&host, &*names, &handle, lane, list, requested);
        self.out.push(message);
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
        let Some(view) = self.view(lane) else {
            return Ok(());
        };
        let value = value.and_then(wire::runtime_value);
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
        let Some(pending) = form::Pending::new(
            self.forms.saturating_add(1),
            (lane, key.to_owned()),
            view,
            aref,
            meta,
        ) else {
            return false;
        };
        self.forms = pending.id;
        self.out.push(ToUi::Form(Some(Box::new(
            pending.shown(&host, &*self.names),
        ))));
        self.form = Some(pending);
        true
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

    /// The view an edit the window made in the view of `lane` showing `target` applies to, and
    /// whether the lane still shows it: the lane's own view while it shows `target`; otherwise
    /// (the lane moved on to another resource before the edit arrived, as a press opening the
    /// next note does) a view of `target` opened for the edit, so that what was typed into a
    /// note lands in that note.
    fn edited(
        &self,
        lane: Lane,
        target: &ui::ResourceRef,
    ) -> Result<(Rc<RefCell<ViewInstance>>, bool), Words> {
        if let Some(shown) = self.lane(lane)
            && wire::resource(&shown.target) == *target
        {
            return Ok((Rc::clone(&shown.view), true));
        }
        let open_failed = || Words::phrase("tilolang-open-failed");
        let host = self.host().ok_or_else(open_failed)?;
        let resource = wire::runtime_resource(target).ok_or_else(open_failed)?;
        let view = host.open_resource(&resource, None).map_err(|error| {
            tracing::warn!(%error, "the resource an edit was made in did not open");
            open_failed()
        })?;
        Ok((Rc::new(RefCell::new(view)), false))
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
        let (view, shown) = self.edited(lane, target)?;
        if shown && let Some(lane) = self.lane_mut(lane) {
            let acked = lane.acked.entry(key.to_owned()).or_default();
            *acked = (*acked).max(seq);
        }
        let current = match view.borrow().bound_value(key) {
            Some(Value::Text(text)) => text,
            _ => String::new(),
        };
        let text = spliced(&current, at as usize, delete as usize, insert);
        self.edit_in(lane, &view, shown, key, Value::Text(text))
    }

    /// Apply an edit of the bound control `key` of the view of `target` in `lane`.
    fn edit(
        &mut self,
        lane: Lane,
        target: &ui::ResourceRef,
        key: &str,
        value: Value,
    ) -> Result<(), Words> {
        let (view, shown) = self.edited(lane, target)?;
        self.edit_in(lane, &view, shown, key, value)
    }

    /// Apply an edit of the bound control `key` of `view`; while `lane` shows it (`shown`),
    /// the window shows what it sent.
    fn edit_in(
        &mut self,
        lane: Lane,
        view: &Rc<RefCell<ViewInstance>>,
        shown: bool,
        key: &str,
        value: Value,
    ) -> Result<(), Words> {
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
        let Some(action) = host.packages().into_iter().find_map(|package| {
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
                    self.pressed_opens.push(made.target.id_string());
                }
            }
            _ => {}
        }
    }

    // ----- frames --------------------------------------------------------------------------

    /// The first half of a frame: bring the runtime up to date with the room record `record`
    /// (`None` while the instance opens over the room's indexes), open the root once the
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
            // Until the indexes are read from, the lists would show nothing: the window shows
            // the platform's loading state.
            if host.index_ready() {
                self.started = true;
                self.loading = false;
                self.open_root(&host);
            } else if !self.loading {
                self.loading = true;
                self.out.push(ToUi::Loading);
            }
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

    /// The second half of a frame: what the person did and what the door answered, run in the
    /// runtime, and what changed sent. Whether anything changed.
    pub fn update(&mut self) -> bool {
        let received = self.receive();
        let door = self.take_door_work();
        let opened = self.take_open_requests();
        let pulled = self.poll_origins();
        let settled = self.settle_outbox();
        let renamed = std::mem::take(&mut self.names_changed);
        if renamed {
            for view in [self.root.as_ref(), self.detail.as_ref()]
                .into_iter()
                .flatten()
            {
                view.view.borrow().refresh();
            }
        }
        // Who is at a resource shows wherever a view reads its lenses.
        let lenses = std::mem::take(&mut self.lenses_changed);
        let members = self.follow_members();
        let changed =
            received || door || opened || pulled || settled || lenses || renamed || members;
        if changed {
            self.poll_views();
        }
        if renamed {
            // A name is read into a node's value when it is sent: every tree is sent again.
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

    /// Give the runtime the workspace's members as the room's policy has them now, when it
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
                *person == standing.person && *policy == standing.policy && *people == self.people
            });
        if seen {
            return false;
        }
        host.runtime().set_members(
            crate::members::members_of(&standing, &self.people),
            Some(crate::members::reads_of(&standing, &self.people, &host)),
        );
        self.members_from = Some((standing.person, standing.policy, self.people.clone()));
        true
    }

    /// Which person each participant presence reports is a device of, `(participant,
    /// person)`, where the room's host paired them: the policy's people are the runtime's
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
        let tree = {
            let instance = view.view.borrow();
            convert_with(&host, &*names, &instance, |convert| {
                convert.node(instance.tree())
            })
        };
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
    /// What the indexes hold reaches the disk as the instance ends: the next window of the room
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

/// Run `run` with the converter of `view`'s trees over `host`, naming people by `names`.
fn convert_with<T>(
    host: &Host,
    names: &dyn NameHost,
    view: &ViewInstance,
    run: impl FnOnce(&Convert<'_>) -> T,
) -> T {
    let field_name = |binding: &Binding| Instance::field_name(host, binding);
    let cases = |id: &str| cases(host, id);
    let toggles = |key: &str, event: &str| toggles(host, view, key, event);
    let invokes = |key: &str, event: &str| invokes(view, key, event);
    let embeds = |reference: &crate::runtime::rich::Reference| inline_view(host, names, reference);
    run(&Convert {
        names,
        field_name: &field_name,
        cases: &cases,
        toggles: &toggles,
        invokes: &invokes,
        embeds: &embeds,
    })
}

/// The tilolang resource a reference in rich text names, when it names one of a package this
/// host has loaded: by its id, of no type yet.
fn referenced_resource(
    host: &Host,
    reference: &crate::runtime::rich::Reference,
) -> Option<ResourceRef> {
    let at = reference.as_resource()?;
    host.package(&at.app)?;
    let id = crate::runtime::rich::resource_id(reference)?;
    ResourceRef::parse_id(id, String::new())
}

/// How a reference in rich text reads for the viewer: a person by what the room calls them; a
/// resource of a package here by its title now, when the viewer holds it, and as only existing
/// when they do not (a reference never grants a read); a resource of another app as its run.
#[must_use]
pub fn inline_view(
    host: &Host,
    names: &dyn NameHost,
    reference: &crate::runtime::rich::Reference,
) -> tilo_render_protocol::patch::rich::InlineView {
    use crate::runtime::Store as _;
    use tilo_render_protocol::patch::rich::InlineView;
    if let Some(person) = reference.as_person() {
        return InlineView::Person {
            identity: person.to_owned(),
            name: names.name(&Principal(person.to_owned())),
        };
    }
    let Some(mut target) = referenced_resource(host, reference) else {
        return InlineView::Elsewhere;
    };
    match host.store().type_of(&target) {
        Ok(ty) => {
            target.ty = ty;
            InlineView::Resource {
                title: host.label_at(&target),
                icon: None,
                detail: None,
            }
        }
        Err(_) => InlineView::Unreadable,
    }
}

/// No names: what a bound value converts with (it holds none).
struct NoNames;

impl NameHost for NoNames {
    fn name(&self, who: &Principal) -> String {
        who.0.clone()
    }
}

/// A converter that reads nothing beside the values it converts.
fn plain(names: &NoNames) -> Convert<'_> {
    Convert {
        names,
        field_name: &|_| String::new(),
        cases: &|_| Vec::new(),
        toggles: &|_, _| None,
        invokes: &|_, _| None,
        embeds: &|_| tilo_render_protocol::patch::rich::InlineView::Elsewhere,
    }
}

/// The values the bound controls under `node` (a node of `view`) hold now.
fn bound_values(node: &UiNode, view: &ViewInstance, out: &mut Vec<(String, ui::Value)>) {
    if node.binding.is_some()
        && let Some(value) = view.bound_value(&node.key)
    {
        let names = NoNames;
        out.push((node.key.clone(), plain(&names).value(&value)));
    }
    for child in &node.children {
        if !ui::is_virtual_list(&node.kind) || child.kind == wire::SLOT {
            bound_values(child, view, out);
        }
    }
}

/// The resource and field the event `event` of the node `key` of `view` toggles, when the
/// action it applies toggles one (`pin` toggles `pinned`).
fn toggles(
    host: &Host,
    view: &ViewInstance,
    key: &str,
    event: &str,
) -> Option<(ResourceRef, String)> {
    let aref = view.event_action(key, event)?;
    let action = host.packages().into_iter().find_map(|package| {
        package
            .meta
            .actions
            .iter()
            .find(|action| action.id == aref.action)
            .cloned()
    })?;
    let field = action.toggles?;
    match aref.args.first() {
        Some(Value::Ref(subject)) => Some((ResourceRef::clone(&subject.target), field)),
        _ => None,
    }
}

/// The resource and the action the event `event` of the node `key` of `view` runs, when it
/// runs an action on a resource: what a menu tells a placed command by.
fn invokes(view: &ViewInstance, key: &str, event: &str) -> Option<(ResourceRef, String)> {
    let aref = view.event_action(key, event)?;
    match aref.args.first() {
        Some(Value::Ref(subject)) => Some((ResourceRef::clone(&subject.target), aref.action)),
        _ => None,
    }
}

/// The window's message of rows `requested` of the list `list` of the view `view` of `lane`.
fn rows_message(
    host: &Host,
    names: &dyn NameHost,
    view: &Rc<RefCell<ViewInstance>>,
    lane: Lane,
    list: &str,
    requested: rows::Requested,
) -> ToUi {
    let instance = view.borrow();
    let rows = convert_with(host, names, &instance, |convert| {
        requested.rows.iter().map(|row| convert.node(row)).collect()
    });
    ToUi::Rows {
        lane,
        list: list.to_owned(),
        len: u32::try_from(requested.len).unwrap_or(u32::MAX),
        start: u32::try_from(requested.start).unwrap_or(u32::MAX),
        keys: requested.keys,
        rows,
    }
}

/// The virtual lists of a view's tree, by key.
fn virtual_lists(tree: &UiNode) -> Vec<String> {
    virtual_list_paths(tree)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
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

/// What a view's patches changed, by where it shows.
#[derive(Default)]
struct Sorted {
    /// The virtual lists' rows, by list key.
    lists: BTreeMap<String, ListChanges>,
    /// Something besides those rows changed.
    content: bool,
}

impl Sorted {
    fn list(&mut self, key: &str) -> &mut ListChanges {
        self.lists.entry(key.to_owned()).or_default()
    }
}

/// Sort a view's `patches` against its virtual lists `lists` (each with its keys from the root):
/// rows inserted, removed or moved, and rows changed, are their lists'; the rest is content.
fn sort(patches: Vec<Patch>, lists: &[(String, Vec<String>)]) -> Sorted {
    let mut sorted = Sorted::default();
    for patch in patches {
        let (path, structural) = match &patch {
            Patch::SetRoot(_) => {
                sorted.content = true;
                for (list, _) in lists {
                    sorted.list(list).reread = true;
                }
                continue;
            }
            Patch::Insert { parent, .. }
            | Patch::Remove { parent, .. }
            | Patch::Move { parent, .. } => (parent.as_slice(), true),
            Patch::SetProp { path, .. }
            | Patch::SetBinding { path, .. }
            | Patch::SetEvent { path, .. }
            | Patch::SetAnchor { path, .. } => (path.as_slice(), false),
        };
        let mut taken = false;
        for (list, at) in lists {
            if structural && path == at.as_slice() {
                sorted.list(list).structure.push(patch.clone());
                taken = true;
                break;
            }
            if path.len() > at.len() && path.starts_with(at) {
                if let Some(row) = path.get(at.len()) {
                    let changes = sorted.list(list);
                    if !changes.rows.contains(row) {
                        changes.rows.push(row.clone());
                    }
                }
                taken = true;
                break;
            }
            if structural && at.starts_with(path) {
                // What holds the list changed: the list may be another now.
                sorted.list(list).reread = true;
            }
        }
        if !taken {
            sorted.content = true;
        }
    }
    sorted
}

/// The other packages of the room whose records the package `app` holds references to.
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
    host.packages().into_iter().find_map(|package| {
        let declared = package
            .meta
            .types
            .iter()
            .find(|declared| declared.id == ty)?
            .clone();
        Some((package.meta, declared))
    })
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

/// The title of a resource: its first `Text` field marked `title`; else, for a package's root,
/// the package's title; else its type's name.
#[must_use]
pub fn title_of(host: &Host, resource: &ResourceRef) -> String {
    let Some((meta, ty)) = type_of(host, &resource.ty) else {
        return String::new();
    };
    let titled = ty
        .fields
        .iter()
        .find(|field| field.role.as_deref() == Some("title"));
    if let Some(field) = titled
        && let Ok(value) = host.runtime().read(resource, &field.id)
    {
        let text = match value {
            Value::Text(text) => text,
            Value::Opt(Some(held)) => match *held {
                Value::Text(text) => text,
                _ => String::new(),
            },
            _ => String::new(),
        };
        if !text.trim().is_empty() {
            return text;
        }
    }
    if resource.item.is_none() && meta.roots.contains(&ty.id) {
        return meta.title;
    }
    sentence(&ty.name)
}

/// The cases of the enum `id`: each case's name, what a control shows for it, and its icon's
/// name.
#[must_use]
pub fn cases(host: &Host, id: &str) -> Vec<wire::Case> {
    host.packages()
        .into_iter()
        .find_map(|package| {
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
