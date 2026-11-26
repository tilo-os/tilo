//! An app instance: one window of a tilolang app as its app host runs it, without the toolkit.
//!
//! The instance holds what the window shows and does, as data: the host over the device's store,
//! the window's two views (the root, and the view of the resource `open(r)` or a row went to),
//! the rows of their virtual lists, and what bound controls show. It sends its window component patches and hears
//! what the person did, over the patch protocol (`tilo_render_protocol::patch`), through any
//! transport: a queue beside a window in one process, the UI host's socket, or `postMessage`
//! from a worker. What builds, lays out, draws and takes input is the window's.
//!
//! Each frame the instance
//!
//! 1. takes what the person did and runs it in the runtime, which writes it to the store as one
//!    attributed, undoable change of the person's,
//! 2. re-runs a view if what it read changed, and sends the window the patches that bring its
//!    copy of the view's tree up to date.
//!
//! What holds for every window is the platform's, and so the instance's: the lenses on each
//! resource are what presence reports; a destructive action a person runs offers Undo; a
//! primary action a person runs shows what it made.

mod convert;
mod facts;
pub mod form;
mod history;
pub use history::WINDOW_HISTORY;
mod patches;
mod person_state;
mod press;
pub use patches::spliced;
use patches::{patch_in_place, show_content_rows, sort, virtual_list_paths};
pub use press::{PressCalls, PressOutcome, Pressed};
pub mod rows;
mod shown;
mod views;

pub use convert::{
    PREVIEW_CHARS, inline_title, inline_view, link_of, preview, published_of, shown_values,
    window_tree,
};
pub(crate) use convert::{bound_values, convert_with, referenced_resource, rows_message};
pub mod wire;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::ActionAddress;
use tilo_render_protocol::patch::{self as ui, Level, ToApp, ToUi, Transport, Words};

use crate::bundle::Bundle;
use crate::host::{Host, Package, RootView};
use crate::resources::ResourceStore;
use crate::runtime::ids::MemberId;
use crate::runtime::meta::{Meta, TypeMeta};
use crate::runtime::{
    ActionRef, Actor, BindTarget, Binding, EditOutcome, InvokeOutcome, NameHost, OutsideSink,
    Patch, RefValue, ResourceRef, UiNode, Value, ViewInstance,
};
use crate::upgrade::Upgrade;
use facts::Facts;
use rows::ListRows;
use tilo_action_policy::PersonId;
use wire::Convert;

/// How long a migration that waits for its turn waits before it is asked for again, in seconds.
const MIGRATE_AGAIN_S: i64 = 5;

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

/// What the person asked of their history at the window: the document's undo or redo
/// (`ToApp::Undo`, `ToApp::Redo`), or an undo a view applied (`undo(v)`, `undo(run)`), as the
/// one who runs the instance carries it out on the document through its history verb `undo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryAsk {
    /// Take back their last step.
    Undo,
    /// Bring back what their last undo took back.
    Redo,
    /// Take back the step the change (a change id of the document) is part of.
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

/// The later builds of one package, as they arrive while its instances run: the latest, and what
/// wakes each instance that follows them. A development session hands them over the host's api;
/// `tilolang dev` without one, from the directory it publishes builds into.
#[derive(Default)]
pub struct LaterBuilds(std::sync::Mutex<Arrived>);

#[derive(Default)]
struct Arrived {
    latest: Option<Update>,
    /// What each following instance is woken by, while it lives.
    wakes: Vec<std::sync::Weak<dyn Fn() + Send + Sync>>,
}

impl LaterBuilds {
    fn arrived(&self) -> std::sync::MutexGuard<'_, Arrived> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The latest later build, once there is one.
    #[must_use]
    pub fn latest(&self) -> Option<Update> {
        self.arrived().latest.clone()
    }

    /// Call `wake` whenever a later build arrives, for as long as it lives.
    pub fn follow(&self, wake: &crate::background::Waker) {
        let mut arrived = self.arrived();
        arrived.wakes.retain(|wake| wake.strong_count() > 0);
        arrived.wakes.push(Arc::downgrade(wake));
    }

    /// Take `bundle` as the latest later build, and wake every instance that follows them.
    pub fn hand_over(&self, bundle: Bundle) {
        let mut arrived = self.arrived();
        let generation = arrived
            .latest
            .as_ref()
            .map_or(1, |update| update.generation.saturating_add(1));
        arrived.latest = Some(Update {
            generation,
            bundle: Arc::new(bundle),
        });
        arrived.wakes.retain(|wake| wake.strong_count() > 0);
        let wakes: Vec<_> = arrived
            .wakes
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        drop(arrived);
        for wake in wakes {
            wake();
        }
    }
}

/// What the instance publishes on the API while it lives: the workspace's documents with their
/// actions, kept up to date change by change.
pub trait Publication {
    /// Bring what is published up to date with the document.
    fn update(&self);
    /// Whether documents are still being read off the frame.
    fn pending(&self) -> bool;
    /// Bring what is published up to date, waiting for the documents being read off the frame:
    /// for an embedder with no frame to take them in on, which reads what is published once it
    /// covers the document. Whether what is published changed.
    fn settle(&self) -> bool {
        self.update();
        false
    }
    /// How many records the updates so far have looked at, one per document per turn: what
    /// publishing a change costs, whatever the workspace holds.
    fn visited(&self) -> u64 {
        0
    }
    /// How many times the updates so far have read every document in full
    /// ([`Publications::sweeps`](crate::publish::Publications::sweeps)).
    fn sweeps(&self) -> u64 {
        0
    }
}

mod platform;

pub use platform::{
    GlobalLocale, Locale, Platform, PlatformBlobs, PlatformClipboard, PlatformFiles, PlatformFonts,
    PlatformWeb, PlatformWords, SharedCanvas, SharedPdf, Zoned, sentence_at, shown_at, wall_moment,
    wall_now,
};
#[cfg(all(not(target_family = "wasm"), feature = "test-support"))]
pub mod testing;

/// How an instance starts.
pub struct Config {
    /// The package the window runs.
    pub app: String,
    /// The person at the window.
    pub me: PersonId,
    pub mode: Mode,
    /// The device that carries out the outside effects the workspace queues.
    pub device: Option<String>,
    /// The store the workspace's resources are read from and written to.
    pub store: ResourceStore,
    /// Where outside effects go; the development log when `None`.
    pub sink: Option<Box<dyn OutsideSink>>,
    /// The root and the view the window opens over, each by name. A package with more than one
    /// root (the shell's `Shell` and its `Layout`) says which layer this window is;
    /// [`RootView::default`] takes the package's first root and its default view, as the
    /// launcher opens an app.
    pub root: RootView,
    /// Where the workspace's resources open on the web, where its embedder knows it: what the
    /// window's `share-link` mints under ([`crate::share`], [`Instance::share_link`]). `None` on
    /// a page no workspace stands behind, and then the link is refused with the system's words.
    pub link_home: Option<crate::share::LinkHome>,
}

/// The names of the workspace, as the runtime asks for them.
#[derive(Clone)]
struct SharedNames(Rc<dyn NameHost>);

impl NameHost for SharedNames {
    fn name(&self, who: &Actor) -> String {
        self.0.name(who)
    }

    fn hue(&self, who: &Actor) -> Option<u8> {
        self.0.hue(who)
    }

    fn picture(&self, who: &Actor) -> Option<String> {
        self.0.picture(who)
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

/// Where an edit the window made applies: the level's own view showing the item, the level's view
/// showing another item of the same list, or a view of the item opened for the edit.
enum Place {
    /// The level's own view, showing the item.
    Here(Rc<RefCell<ViewInstance>>),
    /// The level's own view, drawing another item of its page: `before` is the item it shows.
    Beside {
        view: Rc<RefCell<ViewInstance>>,
        before: ResourceRef,
    },
    /// A view of the item, opened for the edit.
    Fresh(Rc<RefCell<ViewInstance>>),
}

impl Place {
    /// Whether the level shows the item the edit was made in.
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

/// A paragraph a search hit asked the window to show, which the editor of the text holding it
/// shows.
struct Revealing {
    /// The resource whose text holds the paragraph, by its published id, as the window names
    /// what a control is bound to.
    resource: String,
    /// The paragraph, by its place in that text.
    block: crate::search::Block,
}

/// One of the window's two views: a resource shown through a view.
struct Shown {
    target: ResourceRef,
    /// The item of the window's own view this level shows in place (a tree beside what it
    /// shows), when it shows one there: edits to its page apply to the level's own view.
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
    /// The view calls that read the space layout gives them, as last sent: the window measures
    /// these and no others.
    spaces: Option<Vec<String>>,
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
            spaces: None,
        }
    }
}

/// An action the window ran: what it was, what it returned, and the title of what it was run on,
/// read before it ran, as a destructive one takes its target away.
struct Ran {
    aref: ActionRef,
    result: Value,
    target: Option<String>,
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
    me: PersonId,
    mode: Mode,
    device: Option<String>,
    /// Where this window's resources open on the web, when its embedder knows it: what
    /// [`Self::share_link`] mints under.
    link_home: Option<crate::share::LinkHome>,
    store: ResourceStore,
    /// Where the host reads the person's access in the workspace, once its embedder has one.
    access: Rc<RefCell<Option<Rc<crate::authority::AccessSource>>>>,
    /// Where the host reads the workspace's content from, once its embedder has one.
    content: Rc<RefCell<Option<crate::content::ContentSource>>>,
    platform: Rc<dyn Platform>,
    names: Rc<dyn NameHost>,
    host: Rc<RefCell<Option<Host>>>,
    publication: Option<Box<dyn Publication>>,
    root: Option<Shown>,
    detail: Option<Shown>,
    /// The facts page of a resource whose type declares no view, in the detail level ([`facts`]).
    facts: Option<Facts>,
    /// The detail level shows the history of what the window shows ([`history`]), in place of
    /// what it showed before, which it shows again when the history closes.
    window_history: Option<history::Before>,
    /// What the facts page says changed since it was drawn.
    facts_stale: bool,
    /// The space the window last said each view of a level has (`ToApp::Space`), by the view
    /// call's key (`""` for the level's own root): a view opened in the level later starts from
    /// it, and a class follows a boundary only past its hysteresis.
    root_space: HashMap<String, crate::runtime::space::Space>,
    detail_space: HashMap<String, crate::runtime::space::Space>,
    transport: Rc<RefCell<Box<dyn Transport>>>,
    /// The page this app's window shows, when one is asked: its calls go to the window.
    page: Option<Rc<crate::page::PageStation>>,
    /// What waits to be sent this frame.
    out: Vec<ToUi>,
    /// The lenses on each resource of the package, as presence last reported them.
    lenses: HashMap<String, Vec<(Actor, bool, bool)>>,
    lenses_changed: bool,
    /// The workspace's members' resource named a different list since the window was last told
    /// what people are called ([`Self::take_member_names`]).
    member_names_moved: bool,
    /// Who is in each call the call service told of, by the id it names the call's place by.
    calls: HashMap<String, Vec<crate::runtime::CallMember>>,
    /// The microphones each call's view offers, by the place the call is held on: what
    /// `call_inputs(r)` reads.
    call_inputs: HashMap<String, Vec<crate::runtime::CallInput>>,
    /// The steps the person took in the system's calls, in order, not yet taken by the host.
    call_steps: Vec<(ResourceRef, crate::runtime::CallStep)>,
    /// The tilolang lines the person ran from a view, in order, not yet taken by the host.
    line_runs: Vec<(ResourceRef, String)>,
    /// What makes each of the person's presses of an app action a call of its own, once the
    /// host that runs the instance said ([`Self::set_press_calls`]).
    press_calls: Option<Rc<dyn PressCalls>>,
    /// The presses that ran as calls, in order, not yet taken by the host.
    pressed: Vec<Pressed>,
    /// What the window's page started or stopped waiting on the person's yes for, as the window
    /// encoded it, in order, not yet taken by the host.
    page_prompts: Vec<Vec<u8>>,
    /// The person's environment moved: their language, their formats, or what the member list
    /// calls someone. What a view shows of any of them is shown again, and what it sent is sent again.
    person_changed: bool,
    open_requests: Rc<RefCell<Vec<crate::effects::OpenEffect>>>,
    /// Whether the view of a shell places what it opens on the canvas, rather than opening a
    /// detail level of the shell's own window ([`Instance::place_opens`]).
    place_opens: bool,
    /// The resources a view of a shell opened since the last turn: the places the session
    /// arranges on the canvas.
    opens_to_place: Vec<(ResourceRef, Option<String>, bool)>,
    /// What a person's press of a primary action made, to open as they asked for it.
    pressed_opens: Vec<(u64, String)>,
    /// What another action a person chose made, to show where the window shows what it made: a
    /// note an action made opens in the notebook's tree beside it. A kind the window has no place
    /// for is left where it was, never opened as a page of its own.
    pressed_shows: Vec<(u64, String)>,
    /// The paragraph a search hit asked the window to show, until an editor of the text that
    /// holds it is told to.
    revealing: Option<Revealing>,
    /// The item a search hit named inside what the window shows (a page of a document, a mark on
    /// it), until a list of the window that lists it, or what it refers to, selects its row.
    selecting: Option<ResourceRef>,
    /// How many paragraphs were asked for: what tells an ask for the paragraph shown last apart
    /// from the one before, so it is shown again.
    reveals: u64,
    settle_due: bool,
    sink: Option<Box<dyn OutsideSink>>,
    /// The outside services the package's types with an origin are kept in step with.
    origins: crate::origin::Origins,
    /// The undos and redos the person asked for, in order, not yet taken.
    history: Vec<HistoryAsk>,
    /// The window's notice offers to take back the marks the person just moved seen, not the
    /// document's last change: it is about what the window said last.
    seen_offer: bool,
    /// The actions the person chose at the window (a press, the palette), with the press each
    /// is, in order, until the host runs them as the person's own steps.
    chosen: VecDeque<(u64, ActionAddress, Option<String>)>,
    /// Whether a press of the app's own action on one of its own roots is the host's to run
    /// elsewhere ([`Self::run_own_records_elsewhere`]) rather than here.
    own_records_elsewhere: bool,
    /// Those presses, in order, until the host runs them ([`Self::take_own_record_calls`]).
    own_record_calls: Vec<(ActionAddress, serde_json::Map<String, serde_json::Value>)>,
    /// The scenario a capture stages, until the host runs it.
    staged: Option<String>,
    /// The staged scenario's sample data is dated before the story it dates
    /// ([`Instance::stage_story`]).
    story: bool,
    /// Why the scenario a capture staged did not run whole, until the capture takes it: a
    /// capture of a half-run scenario is no picture of it.
    stage_failed: Option<String>,
    /// Why the app's root did not open, until the host that opened it takes it: a window with no
    /// root view stands empty, and shows why it does instead.
    open_failed: Option<String>,
    started: bool,
    /// The package has no root of its own to open: everything it shows is opened by its
    /// embedder ([`Instance::without_root`]).
    rootless: bool,
    loading: bool,
    moved: bool,
    /// Whether the package's migrations ran over the workspace document since the window opened,
    /// and none waits for its turn.
    migrated: bool,
    /// When the migrations that wait for their turn are asked for again, in seconds since the
    /// epoch.
    migrate_again_at: Option<i64>,
    /// The later build this instance runs, 0 for the installed one.
    generation: u64,
    follows: bool,
    /// A later build this instance took, until its embedder asks.
    taken_build: Option<u64>,
    /// The window's keyboard focus: the bound control it is in.
    focus: Option<(Level, String)>,
    composing: bool,
    /// The modifier keys the window last said are held: what a press's `host.Press` reads.
    held: ui::Held,
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
    /// How many times the window asked for the rows of one of its lists (`ToApp::Rows`): what a
    /// scroll costs the window in round trips to the instance.
    rows_asked: u64,
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
        let host: Rc<RefCell<Option<Host>>> = Rc::default();
        let names: Rc<dyn NameHost> =
            Rc::new(crate::names::HostNames::new(names, Rc::clone(&host)));
        Self {
            app: config.app,
            root_view: config.root,
            me: config.me,
            mode: config.mode,
            device: config.device,
            link_home: config.link_home,
            store: config.store,
            access: Rc::default(),
            content: Rc::default(),
            platform,
            names,
            host,
            publication: None,
            root: None,
            detail: None,
            facts: None,
            window_history: None,
            facts_stale: false,
            root_space: HashMap::new(),
            detail_space: HashMap::new(),
            transport: Rc::new(RefCell::new(transport)),
            page: None,
            out: Vec::new(),
            lenses: HashMap::new(),
            lenses_changed: false,
            member_names_moved: false,
            calls: HashMap::new(),
            call_inputs: HashMap::new(),
            call_steps: Vec::new(),
            line_runs: Vec::new(),
            press_calls: None,
            pressed: Vec::new(),
            page_prompts: Vec::new(),
            person_changed: false,
            open_requests: Rc::default(),
            place_opens: false,
            opens_to_place: Vec::new(),
            pressed_opens: Vec::new(),
            pressed_shows: Vec::new(),
            revealing: None,
            selecting: None,
            reveals: 0,
            settle_due: false,
            sink: Some(
                config
                    .sink
                    .unwrap_or_else(|| Box::new(crate::sink::LogSink::dev())),
            ),
            origins: Rc::default(),
            history: Vec::new(),
            seen_offer: false,
            chosen: VecDeque::new(),
            own_records_elsewhere: false,
            own_record_calls: Vec::new(),
            staged: None,
            story: false,
            stage_failed: None,
            open_failed: None,
            started: false,
            rootless: false,
            loading: false,
            moved: false,
            migrated: false,
            migrate_again_at: None,
            generation: 0,
            follows: false,
            taken_build: None,
            focus: None,
            composing: false,
            held: ui::Held::default(),
            fired: 0,
            last_fire: 0,
            taken_press: 0,
            ran: 0,
            ran_ahead: BTreeSet::new(),
            fired_told: 0,
            patches_sent: 0,
            rows_asked: 0,
            form: None,
            forms: 0,
        }
    }

    /// This instance on the device `device`: the one that carries out the outside effects its
    /// workspace queues. Before the host starts.
    pub fn set_device(&mut self, device: Option<String>) {
        self.device = device;
    }

    /// Run a package that has no root view of its own: what it shows is opened in a level by its
    /// embedder ([`Self::open_with`]), as a view of another app's state is. Before the host
    /// starts.
    pub fn without_root(&mut self) {
        self.rootless = true;
    }

    /// Read the person's access in the workspace from `source`: what the runtime invokes is decided
    /// by the workspace's policy with it. May come after the host started (a window shown before its
    /// host gave it one).
    pub fn set_access(&self, source: Rc<crate::authority::AccessSource>) {
        *self.access.borrow_mut() = Some(source);
    }

    /// Read the workspace's content from `source`: its owners' limits and the content a limit
    /// refused, which the host serves as `workspace.content` each time [`Host::follow_content`]
    /// runs. May come after the host started.
    pub fn set_content_source(&self, source: crate::content::ContentSource) {
        if let Some(host) = self.host.borrow().as_ref() {
            host.set_content_source(Rc::clone(&source));
        }
        *self.content.borrow_mut() = Some(source);
    }

    /// Send the outside effects the instance settles to `sink`. Before the host starts.
    pub fn set_sink(&mut self, sink: Box<dyn OutsideSink>) {
        self.sink = Some(sink);
    }

    /// Keep the package's types with an origin in step with `origin`'s service, and hand it the
    /// outside effects of its capabilities. One origin of each name stands: a window whose view is
    /// built again hands the origin it already has, and never a second one that would ask the
    /// service for the same effects beside it.
    pub fn add_origin(&mut self, origin: crate::origin::SharedOrigin) {
        let name = origin.borrow().name().to_owned();
        let mut origins = self.origins.borrow_mut();
        if let Some(held) = origins.iter_mut().find(|held| held.borrow().name() == name) {
            *held = origin;
        } else {
            origins.push(origin);
        }
    }

    /// Poll each origin: what the services said is written and handed to the API.
    fn poll_origins(&mut self) -> bool {
        if self.mode != Mode::Live
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
            self.commit_planned();
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
    /// its own chrome relabels with the next frame it is sent, and every view that shows a word
    /// is drawn again.
    pub fn speak(&mut self, locale: String) {
        self.out.push(ToUi::Speak(locale));
        self.person_changed = true;
    }

    /// What search hits and agents asked the window to show, until it shows them.
    #[must_use]
    pub fn open_requests(&self) -> Rc<RefCell<Vec<crate::effects::OpenEffect>>> {
        Rc::clone(&self.open_requests)
    }

    /// The link to the resource `resource` of this window's app, minted under the home this
    /// window's embedder handed it ([`Config::link_home`]): what a page's own copy link asks,
    /// answered by the same [`crate::share::answer`] that mints a native window's palette link.
    ///
    /// # Errors
    /// Why no link can be made: no workspace page stands behind this window, or the resource is
    /// not one of this app's.
    pub fn share_link(&self, resource: &str) -> Result<tilo_actions::ActionOutput, String> {
        use crate::meta::MetaExt as _;
        use tilolang_runtime::store::Store as _;
        let host = self.host().ok_or("the app has not started")?;
        let app = crate::publish::app_of(resource)
            .ok_or_else(|| format!("`{resource}` is no resource of an app"))?;
        let package = host
            .package(app)
            .ok_or_else(|| format!("`{app}` is not loaded here"))?;
        let path = resource
            .strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
            .map(|(_, path)| path)
            .ok_or_else(|| format!("`{resource}` is no resource of an app"))?;
        let probe = ResourceRef::parse_id(path, String::new())
            .ok_or_else(|| format!("`{resource}` names no resource"))?;
        let ty = host
            .store()
            .type_of(&probe)
            .map_err(|error| error.to_string())?;
        let type_name = package
            .meta
            .type_named(&ty)
            .map_or_else(|| ty.clone(), |declared| declared.name.clone());
        let primary = package.meta.default_view(&ty).map(str::to_owned);
        let views = package
            .meta
            .views_showing(&type_name)
            .into_iter()
            .map(|view| tilo_resource::share::SharedView {
                id: view.id.clone(),
                primary: primary.as_deref() == Some(view.id.as_str()),
                range: false,
                desktop: false,
            })
            .collect();
        let request = tilo_resource::share::ShareRequest {
            resource: resource.to_owned(),
            kind: package.meta.kind_of(&type_name),
            views,
            invite: false,
            args: serde_json::Map::new(),
        };
        let payload = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        crate::share::answer(&payload, app, self.link_home.as_ref())
    }

    /// Show what the person's own step made, as a press of the action that made it does: it
    /// comes to the front as their input does, and takes the keyboard.
    pub fn opens_what_it_made(&mut self, anchor: String) {
        self.pressed_opens.push((self.taken_press, anchor));
    }

    /// Show what another action the person chose made, where the window shows the kind of thing
    /// it is: a note an action made opens in the notebook's tree it stands in. The window's view
    /// decides; a kind it has no place for is left as it was, since only a primary action opens
    /// what it made as a page of its own.
    pub fn shows_what_it_made(&mut self, anchor: String) {
        self.pressed_shows.push((self.taken_press, anchor));
    }

    /// The host, once the instance opened it.
    #[must_use]
    pub fn host(&self) -> Option<Host> {
        self.host.borrow().clone()
    }

    /// How many records the instance's publication has looked at so far, one per document per
    /// turn ([`Publication::visited`]).
    #[must_use]
    pub fn visited_documents(&self) -> u64 {
        self.publication
            .as_ref()
            .map_or(0, |publication| publication.visited())
    }

    /// How many times the instance's publication has read every document in full
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

    /// How many times the window asked for the rows of one of its lists so far.
    #[must_use]
    pub fn rows_asked(&self) -> u64 {
        self.rows_asked
    }

    /// How long from the platform's clock now until the time a view of the window read
    /// (`today` in a query or a filter) next reads differently: the embedder wakes the instance
    /// then (a frame's deadline, its wait's timeout), and that frame runs again exactly the
    /// views that read it. Zero once it has. `None` when no view read the time: nothing about
    /// the time ever wakes the instance.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        [
            self.views_wake_in(),
            self.held_in(),
            self.trigger_in(),
            self.migrate_in(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// How long until the migrations that wait for their turn are asked for again.
    fn migrate_in(&self) -> Option<std::time::Duration> {
        let at = self.migrate_again_at.filter(|_| !self.migrated)?;
        let now = self.now()?;
        Some(std::time::Duration::from_secs(
            u64::try_from(at.saturating_sub(now)).unwrap_or(0),
        ))
    }

    /// How long until a trigger of this app next has something to run, or a secret the window
    /// shows is to be hidden again: what a window that runs the app's triggers waits for. `None`
    /// when neither will come due, or this window does not run them.
    fn trigger_in(&self) -> Option<std::time::Duration> {
        if self.mode != Mode::Live {
            return None;
        }
        let now = self.now()?;
        let trigger = self.host().and_then(|host| host.next_trigger(now));
        let at = match (trigger, crate::page::next_lapse()) {
            (Some(trigger), Some(lapse)) => trigger.min(lapse),
            (trigger, lapse) => trigger.or(lapse)?,
        };
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

    /// The resources of other apps this window's views read, each once: what its host follows
    /// while a view shows one — a reading keyed by a range, say — and what it stops following
    /// once no view does.
    #[must_use]
    pub fn foreign_reads(&self) -> Vec<crate::runtime::ResourceRef> {
        let mut reads: Vec<crate::runtime::ResourceRef> = Vec::new();
        for shown in [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
        {
            for target in shown.view.borrow().foreign_reads() {
                if !reads.contains(&target) {
                    reads.push(target);
                }
            }
        }
        reads
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

    /// The view of `level`, to read it.
    #[must_use]
    pub fn view(&self, level: Level) -> Option<Rc<RefCell<ViewInstance>>> {
        self.level(level).map(|view| Rc::clone(&view.view))
    }

    /// The keys of the rows the view of `level` renders now in its list `list`.
    #[must_use]
    pub fn shown_rows(&self, level: Level, list: &str) -> Vec<String> {
        let mut shown: Vec<String> = self
            .level(level)
            .and_then(|view| view.lists.get(list))
            .map(|rows| rows.shown().iter().cloned().collect())
            .unwrap_or_default();
        shown.sort();
        shown
    }

    /// The undos and redos the person asked for since last asked, in the order they asked: Undo
    /// on a notice, and the window's undo and redo.
    pub fn take_history(&mut self) -> Vec<HistoryAsk> {
        std::mem::take(&mut self.history)
    }

    /// The steps the person took in the system's calls since last asked, in order: each the
    /// resource the call is held on and what they did there. The host carries them out through
    /// the call service.
    pub fn take_call_steps(&mut self) -> Vec<(ResourceRef, crate::runtime::CallStep)> {
        std::mem::take(&mut self.call_steps)
    }

    /// The tilolang lines the person ran from a view since the host last took them, each with the
    /// resource it was run from: the host runs each through the session's program host.
    pub fn take_line_runs(&mut self) -> Vec<(ResourceRef, String)> {
        std::mem::take(&mut self.line_runs)
    }

    /// Run each press of the person's of an app action (a control's event, a form submitted) as
    /// the call `calls` names for it.
    pub fn set_press_calls(&mut self, calls: Rc<dyn PressCalls>) {
        self.press_calls = Some(calls);
    }

    /// Run no press of the app's own action on one of its own roots here: the host keeps the
    /// app's records in the person's workspace and runs each such press there, as the person's
    /// call of that action ([`Self::take_own_record_calls`]). What it changes comes back as the
    /// store moves, so every device of theirs, an agent and a web page change the one record.
    pub fn run_own_records_elsewhere(&mut self) {
        self.own_records_elsewhere = true;
    }

    /// The presses of the app's own actions on its own roots since the host last took them, in
    /// order: each the address its root publishes the action at and its arguments by name.
    pub fn take_own_record_calls(
        &mut self,
    ) -> Vec<(ActionAddress, serde_json::Map<String, serde_json::Value>)> {
        std::mem::take(&mut self.own_record_calls)
    }

    /// The call `aref` is on the dispatch when it is the app's own action on one of its own
    /// roots that changes nothing but the app's records (`press::own_record_call`).
    #[must_use]
    pub fn own_record_call(
        &self,
        aref: &ActionRef,
    ) -> Option<(ActionAddress, serde_json::Map<String, serde_json::Value>)> {
        press::own_record_call(&self.host()?, &self.app, aref)
    }

    /// Whether a press of `aref` was handed to the host to run elsewhere: the app's own action on
    /// its own records, while the host runs those where the records are kept.
    fn handed_elsewhere(&mut self, aref: &ActionRef) -> bool {
        if !self.own_records_elsewhere {
            return false;
        }
        let Some(call) = self.own_record_call(aref) else {
            return false;
        };
        self.own_record_calls.push(call);
        true
    }

    /// The presses that ran as calls since the host last took them, in order: the host records
    /// how each came out with the dispatch.
    pub fn take_pressed(&mut self) -> Vec<Pressed> {
        std::mem::take(&mut self.pressed)
    }

    /// Ask the window's page `request`, as an action's `page.call` does, outside any run: how the
    /// host tells the page no once a call it raised ended without the person's yes.
    ///
    /// # Errors
    /// The window shows no page yet, or did not answer.
    pub fn ask_page(&self, request: &str) -> Result<String, String> {
        use tilolang_runtime::page::PageHost as _;
        let page = self
            .page
            .as_ref()
            .ok_or_else(|| "this window shows no page yet".to_owned())?;
        crate::page::SharedPage(Rc::clone(page)).call(request)
    }

    /// What the window's page started or stopped waiting on the person's yes for since the host
    /// last took it, each as the window encoded it: the host raises each as the app's call of
    /// its own action, and withdraws it once the page no longer waits.
    pub fn take_page_prompts(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.page_prompts)
    }

    /// The invocations the instance's views read (`invoked(id)`), by the id they read each by:
    /// what the host reads the records of.
    #[must_use]
    pub fn invoked_read(&self) -> Vec<String> {
        self.host()
            .map(|host| host.runtime().invoked_read())
            .unwrap_or_default()
    }

    /// How each invocation the views read stands, as the host read its record: `None` for one
    /// whose record names nothing.
    pub fn set_invoked(&mut self, id: &str, invoked: Option<&crate::runtime::Invoked>) {
        if let Some(host) = self.host() {
            host.runtime().set_invoked(id, invoked);
        }
    }

    /// The lines the person ran from the resource whose own id is `place`, as they stand now.
    pub fn set_lines(&mut self, place: &str, lines: &[crate::runtime::LineRun]) {
        if let Some(host) = self.host() {
            host.runtime().set_lines(place, lines);
        }
    }

    /// Whether a view of the instance read who is in a call: its host follows the system's calls
    /// from then on.
    #[must_use]
    pub fn reads_calls(&self) -> bool {
        self.host().is_some_and(|host| host.runtime().reads_calls())
    }

    /// Who is in each call the call service told of now, by the id it names each call's place by
    /// (`call_members(r)` of the resource that id is the own id of): a call not named is empty.
    pub fn set_calls(&mut self, now: HashMap<String, Vec<crate::runtime::CallMember>>) {
        if now == self.calls {
            return;
        }
        if let Some(host) = self.host() {
            for gone in self.calls.keys().filter(|key| !now.contains_key(*key)) {
                host.runtime().set_call(gone, &[]);
            }
            for (place, members) in &now {
                if self.calls.get(place) != Some(members) {
                    host.runtime().set_call(place, members);
                }
            }
        }
        self.calls = now;
        self.lenses_changed = true;
    }

    /// The microphones each call the call service told of can send, by the place it is held on:
    /// what `call_inputs(r)` reads. A call not named offers none.
    pub fn set_call_inputs(&mut self, now: HashMap<String, Vec<crate::runtime::CallInput>>) {
        if now == self.call_inputs {
            return;
        }
        if let Some(host) = self.host() {
            for gone in self
                .call_inputs
                .keys()
                .filter(|key| !now.contains_key(*key))
            {
                host.runtime().set_call_inputs(gone, &[]);
            }
            for (place, inputs) in &now {
                if self.call_inputs.get(place) != Some(inputs) {
                    host.runtime().set_call_inputs(place, inputs);
                }
            }
        }
        self.call_inputs = now;
        self.lenses_changed = true;
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
    pub fn focus(&self) -> (Option<&(Level, String)>, bool) {
        (self.focus.as_ref(), self.composing)
    }

    /// Run the scenario `scenario` once the host runs, as a capture stages it.
    pub fn stage(&mut self, scenario: Option<&str>) {
        self.staged = scenario.map(str::to_owned);
        self.stage_failed = None;
        self.story = false;
    }

    /// Run the scenario `scenario` once the host runs, as a capture of the story it tells
    /// stages it: what it writes without a time of its own is its sample data, the workspace
    /// the story happens to, and is dated a second before the earliest change it dates.
    pub fn stage_story(&mut self, scenario: Option<&str>) {
        self.stage(scenario);
        self.story = true;
    }

    /// Why the scenario [`stage`](Self::stage) named failed, or a package it uses failed its
    /// first scenario, once the host ran it: a capture refuses to picture a half-staged workspace.
    pub fn take_stage_failure(&mut self) -> Option<String> {
        self.stage_failed.take()
    }

    /// Why the app's root did not open, once the host ran it: a window opens on it, so a host
    /// that took no root shows why rather than an empty window.
    pub fn take_open_failure(&mut self) -> Option<String> {
        self.open_failed.take()
    }

    /// The person's environment changed — the language their window speaks, their formats, or
    /// the name the window calls someone: every view that shows what only the window knows shows
    /// it again, and no other boundary runs.
    pub fn person_changed(&mut self) {
        self.person_changed = true;
    }

    fn level(&self, level: Level) -> Option<&Shown> {
        match level {
            Level::Root => self.root.as_ref(),
            Level::Detail => self.detail.as_ref(),
        }
    }

    fn level_mut(&mut self, level: Level) -> Option<&mut Shown> {
        match level {
            Level::Root => self.root.as_mut(),
            Level::Detail => self.detail.as_mut(),
        }
    }

    // ----- the host ------------------------------------------------------------------------

    /// The host over the store, built with the package the first time.
    fn host_for(&mut self) -> Option<Host> {
        if let Some(host) = self.host.borrow().as_ref() {
            // Opened on what an earlier version wrote, the package brings it to where it keeps
            // it now; what that writes is written as any change is.
            // Each migration of a resource is the workspace's once: one that another device runs,
            // or that none may run yet, is asked for again a little later.
            let now = self.now();
            if !self.migrated
                && self.mode == Mode::Live
                && self
                    .migrate_again_at
                    .is_none_or(|at| now.is_some_and(|now| now >= at))
            {
                if host.migrate() > 0 {
                    self.moved = true;
                }
                if host.migrations_wait() {
                    self.migrate_again_at = now.map(|now| now.saturating_add(MIGRATE_AGAIN_S));
                } else {
                    self.migrated = true;
                }
            }
            return Some(host.clone());
        }
        let access = Rc::clone(&self.access);
        let built = Host::open(
            self.store.clone(),
            self.me_actor(),
            self.device.as_deref(),
            move || access.borrow().as_ref().and_then(|source| source()),
        );
        let host = match built {
            Ok(host) => host,
            Err(error) => {
                tracing::error!(%error, "the tilolang runtime could not be set up");
                return None;
            }
        };
        if let Some(source) = self.content.borrow().as_ref() {
            host.set_content_source(Rc::clone(source));
        }
        host.runtime()
            .set_name_host(SharedNames(Rc::clone(&self.names)));
        let runtime = host.runtime();
        runtime.set_shown_host(PlatformShown(Rc::clone(&self.platform)));
        let platform = Rc::clone(&self.platform);
        runtime.set_sentence_by(move |kind, value| platform.sentence(kind, value));
        let platform = Rc::clone(&self.platform);
        host.set_clock(move || platform.now());
        host.runtime()
            .set_web(PlatformWeb(Rc::clone(&self.platform)));
        host.runtime()
            .set_blobs(PlatformBlobs(Rc::clone(&self.platform)));
        host.runtime()
            .set_clipboard(PlatformClipboard(Rc::clone(&self.platform)));
        // The page this window shows: a `page.call` is one message to it and one answer back.
        let page = Rc::new(crate::page::PageStation::new(Rc::clone(&self.transport)));
        host.runtime()
            .set_page(crate::page::SharedPage(Rc::clone(&page)));
        self.page = Some(page);
        if let Some(pdf) = self.platform.pdf() {
            host.runtime().set_pdf(SharedPdf(pdf));
        }
        if let Some(canvas) = self.platform.canvas() {
            host.runtime().set_canvas(SharedCanvas(canvas));
        }
        host.runtime()
            .set_fonts(PlatformFonts(Rc::clone(&self.platform)));
        host.runtime()
            .set_device_files(PlatformFiles(Rc::clone(&self.platform)));
        host.runtime()
            .set_engine(crate::instance::platform::PlatformEngine(Rc::clone(
                &self.platform,
            )));
        host.runtime()
            .set_vault(crate::instance::platform::PlatformVault(Rc::clone(
                &self.platform,
            )));
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
        self.generation = self.platform.load(&host);
        if let Some(builds) = self.platform.builds()
            && self.mode != Mode::Past
            && !self.follows
        {
            self.follows = true;
            builds.follow(&self.platform.waker());
        }
        if self.mode != Mode::Past {
            // What is published and found of the workspace is read off the frame the first time;
            // the instance takes it in when it is ready.
            let _phase = tilo_resource::profiling::Phase::start("tl.publication.start");
            self.publication = self.platform.publish(&host);
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
                .and_then(|ty| {
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
    /// in place when it reads the state the old one wrote, and otherwise by starting over with
    /// it, as the app's next start would. The host the instance runs on afterwards.
    fn follow_updates(&mut self, host: Host) -> Host {
        let Some(update) = self
            .platform
            .builds()
            .and_then(|builds| builds.latest())
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
                let old = host.clone();
                self.start_over().unwrap_or_else(|| {
                    *self.host.borrow_mut() = Some(old.clone());
                    old
                })
            }
        };
        self.platform.surface_changed(&self.app, &update.bundle);
        self.taken_build = Some(update.generation);
        host
    }

    /// Open the root and the detail again on the package `host` now runs: the same resources,
    /// so the same windows and their view state.
    fn reopen(&mut self, host: &Host) {
        // A history the window showed closes: the level shows what it showed before it.
        let detail = match self.leave_history() {
            Some(before) => {
                if before.is_none() {
                    self.out.push(ToUi::Close);
                }
                before
            }
            None => self
                .detail
                .as_ref()
                .map(|view| (view.target.clone(), view.with.clone())),
        };
        let (Ok(target), Ok(view)) = (
            host.root_of_layer(&self.app, &self.root_view),
            host.open_layer(&self.app, &self.root_view),
        ) else {
            tracing::warn!(app = self.app, "the new build has no root to open");
            return;
        };
        self.root = Some(Shown::new(target, self.spaced(Level::Root, view)));
        self.send_open(Level::Root, None);
        self.detail = detail.and_then(|(target, with)| {
            let view = host.open_resource(&target, with.as_deref()).ok()?;
            let mut view = Shown::new(target, self.spaced(Level::Detail, view));
            view.with = with;
            Some(view)
        });
        if self.detail.is_some() {
            self.send_open(Level::Detail, None);
        } else {
            self.reopen_facts();
        }
        self.lenses_changed = true;
        // What is published and offered follows the new build's declarations.
        if self.publication.take().is_some() {
            self.publication = self.platform.publish(host);
        }
        self.commit_planned();
    }

    /// Start over: a new runtime that loads the latest build and reads the store afresh, as a
    /// start of the app does. View state and the open detail start over; every resource is the
    /// one the instance had.
    fn start_over(&mut self) -> Option<Host> {
        self.publication = None;
        self.root = None;
        self.leave_history();
        if self.detail.take().is_some() | self.facts.take().is_some() {
            self.out.push(ToUi::Close);
        }
        *self.host.borrow_mut() = None;
        if self.sink.is_none() {
            self.sink = Some(Box::new(crate::sink::LogSink::dev()));
        }
        self.started = false;
        self.store.moved();
        self.host_for()
    }

    // ----- presence ------------------------------------------------------------------------

    /// Bring the lenses the views read up to date with presence: who else is at each resource
    /// (`peers`, by resource id), and the person themselves at the detail view's resource.
    /// Someone composing a message in the detail view is a typist there: `composers` names
    /// them, and their lens reads `composing` (`lenses(c)`), so a conversation can say
    /// "… is typing" without reading a draft.
    pub fn set_peers_and_composers(
        &mut self,
        peers: HashMap<String, MemberId>,
        composers: &[MemberId],
    ) {
        let mut now: HashMap<String, Vec<(Actor, bool, bool)>> = HashMap::new();
        for (resource, who) in peers {
            let typing = composers.contains(&who);
            now.entry(resource)
                .or_default()
                .push((Actor::Member(who), false, typing));
        }
        if let Some(detail) = self.detail_target() {
            now.entry(detail.id_string())
                .or_default()
                .push((self.me_actor(), true, false));
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
        let Some((scenario, seed)) = host.package(&self.app).and_then(|package| {
            let scenarios = &package.meta.scenarios;
            let found = scenarios
                .iter()
                .find(|scenario| scenario.name == name)
                .cloned()?;
            let seed = scenarios.first().is_some_and(|first| first.name == name);
            Some((found, seed))
        }) else {
            self.stage_failed = Some(format!("{} has no scenario \"{name}\"", self.app));
            return;
        };
        // What the scenarios write without a time of their own is their sample data, and what
        // they date is the story they tell: the store dates the one before the other.
        let now = self.platform.now();
        if self.story {
            host.store().hold_sample(&now);
        }
        // The packages this one points into (a board's apps) are staged first, each with its
        // first scenario, so a scenario that attaches their roots shows them filled. A first
        // scenario is a seed: what its outside services hold is taken in through their origins
        // before it, as a seeded workspace takes it in.
        let mut failed = Vec::new();
        for used in used_packages(host, &self.app) {
            if let Some(first) = used.meta.scenarios.first() {
                let staged = crate::origin::take_in_samples(host, &used.meta.app, &now)
                    .and_then(|()| self.run_scenario(host, &used.meta.app, &first.action));
                if let Err(why) = staged {
                    failed.push(format!("{} \"{}\": {why}", used.meta.app, first.name));
                }
            }
        }
        if seed && let Err(why) = crate::origin::take_in_samples(host, &self.app, &now) {
            failed.push(format!("{} \"{name}\": {why}", self.app));
        }
        let shown = match self.run_scenario(host, &self.app.clone(), &scenario.action) {
            Ok(shown) => shown,
            Err(why) => {
                failed.push(format!("{} \"{name}\": {why}", self.app));
                None
            }
        };
        if let Err(error) = host.store().release_sample() {
            failed.push(format!(
                "the store refused what the scenario wrote: {error}"
            ));
        }
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
            .invoke_action(&self.me_actor(), (action, Vec::new()))
        {
            Ok(InvokeOutcome::Done(Value::Ref(shown))) => {
                Ok(Some(Rc::unwrap_or_clone(shown.target)))
            }
            Ok(InvokeOutcome::Done(_)) => Ok(None),
            Ok(other) => Err(format!("it did not run: {other:?}")),
            Err(error) => Err(error.to_string()),
        }
    }

    // ----- the API ------------------------------------------------------------------------

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

    /// What the person asked for did not happen, for `why`, and the window says so: the
    /// document's changes stay on this device (the workspace refused them for good), an undo or
    /// redo failed (somebody else having changed the document since is never a failure: undo
    /// leaves their newer value), or the action they chose from the window's command palette
    /// did not run.
    pub fn failed(&mut self, why: String) {
        self.notice(Words::Plain(why), ui::NoticeKind::Error);
        self.flush();
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
        let shows = std::mem::take(&mut self.pressed_shows);
        if anchors.is_empty() && shows.is_empty() {
            return false;
        }
        let Some(host) = self.host() else {
            return false;
        };
        for (open, front, press) in anchors {
            // A hit names the published resource (`tl:<app>:<id>`) or the resource's id, and
            // the paragraph of its text to show when it is a hit on one.
            let (anchor, block) = crate::search::split_anchor(open.anchor());
            let anchor = anchor
                .strip_prefix("tl:")
                .and_then(|rest| rest.split_once(':'))
                .map_or(anchor, |(_, id)| id);
            match host.open_plan(&self.app, anchor, open.view()) {
                Ok(plan) => {
                    // The paragraph is shown by the editor of the text that holds it, wherever
                    // the window draws that, once the window shows the resource.
                    let in_text = block.is_some();
                    self.revealing = block.map(|block| Revealing {
                        resource: crate::publish::resource_id(
                            &plan.app,
                            &tilolang_runtime::value::flat_id(anchor),
                        ),
                        block,
                    });
                    // A hit inside what the window shows (a page of the document, not the
                    // document) selects its row there, once a list of the window lists it.
                    self.selecting = plan
                        .target
                        .as_ref()
                        .filter(|target| !in_text && target.id_string() != anchor)
                        .and_then(|_| host.store().type_of_id(anchor))
                        .and_then(|ty| ResourceRef::parse_id(anchor, ty));
                    // The app's own root in its default view is what the window shows already. A
                    // record of another app's reading (a Windows program) is no root of this app,
                    // though the window shows it.
                    let own_root = |target: &ResourceRef| {
                        plan.app == self.app
                            && target.item.is_none()
                            && !tilolang_runtime::foreign::is_foreign(target)
                    };
                    if let Some(target) = plan.target
                        && (!own_root(&target) || plan.view.is_some())
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
        // What a chosen action made shows where the window shows its kind, and nowhere else: a
        // kind the window has no place for leaves the window as it was.
        for (press, anchor) in shows {
            let anchor = anchor
                .strip_prefix("tl:")
                .and_then(|rest| rest.split_once(':'))
                .map_or(anchor.as_str(), |(_, id)| id);
            match host.open_plan(&self.app, anchor, None) {
                Ok(plan) => {
                    if let Some(target) = plan.target
                        && plan.app == self.app
                        && plan.view.is_none()
                        && self.show_in_place(&target)
                    {
                        self.out.push(ToUi::Front {
                            front: ui::Front::Input,
                            shows: Some((press, wire::resource(&target))),
                        });
                    }
                    self.commit_planned();
                }
                Err(error) => {
                    tracing::warn!(%error, "what an action made named nothing in the workspace");
                }
            }
        }
        // Showing an item runs the view's answer for it (`on show`), which may itself open a
        // page (`open(x)`): that ask is taken in this same step, so the page a primary action
        // made opens in the frame that ran it.
        self.follow_opens();
        true
    }

    fn settle_effects(&mut self) -> bool {
        // What waits in the document is settled once the instance reads the document.
        if !std::mem::take(&mut self.settle_due) || self.mode != Mode::Live {
            return false;
        }
        let Some(host) = self.host() else {
            return false;
        };
        match host.settle_effects() {
            Ok(settled) => {
                // What was marked as being handed over is handed over by the next settle,
                // which nothing else would ask for.
                self.settle_due |= settled.owes_settle;
                if settled.written == 0 {
                    return false;
                }
                self.commit_planned();
                true
            }
            Err(error) => {
                tracing::warn!(%error, "outside effects could not be settled");
                false
            }
        }
    }

    /// What follows the runtime's writes: a write the store refused is said, and the outside
    /// effects they queued are settled.
    pub fn commit_planned(&mut self) {
        if self.mode == Mode::Past {
            return;
        }
        if let Some(why) = self.store.take_refusal() {
            self.notice(Words::Plain(why), ui::NoticeKind::Error);
        }
        self.settle_due = true;
    }

    /// The API refused an action the window asked it to run: say why. Whether it was one of
    /// this window's.
    pub fn answered(&mut self, address: &ActionAddress, error: Option<String>) -> bool {
        if let Some(error) = error
            && address.resource.starts_with("tl:")
        {
            self.notice(Words::Plain(error), ui::NoticeKind::Error);
            self.flush();
            return true;
        }
        false
    }

    // ----- what the person did -------------------------------------------------------------

    /// Take what the window sent and do it. Whether anything was.
    fn receive(&mut self) -> bool {
        let mut changed = false;
        // What arrived while a page call waited for its answer, in the order it came.
        let waited = self
            .page
            .as_ref()
            .map(|page| page.waiting())
            .unwrap_or_default();
        for message in waited {
            changed = true;
            self.event(message);
        }
        loop {
            let batch = match self.transport.borrow_mut().recv() {
                Ok(Some(batch)) => batch,
                // Nothing more, or the window closed: the instance ends on this turn.
                Ok(None) | Err(tilo_render_protocol::WireError::Closed) => break,
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

    /// Layout gave the view of `level` at `key` — the level's own root, or a view call by its
    /// `fragment`'s key — `width` by `height` rem of the window: its `space` follows, with a
    /// class changing only past its boundary by the hysteresis, and the boundary runs again only
    /// when something it read of it changed.
    fn offer_space(&mut self, level: Level, key: &str, width: f64, height: f64, rem: f64) {
        let spaces = match level {
            Level::Root => &mut self.root_space,
            Level::Detail => &mut self.detail_space,
        };
        let space = spaces.get(key).map_or_else(
            || crate::runtime::space::Space::from_rem(width, height).with_rem(rem),
            |before| before.followed(width, height).with_rem(rem),
        );
        spaces.insert(key.to_owned(), space);
        if let Some(view) = self.view(level) {
            view.borrow_mut().set_space(key, space);
        }
    }

    /// `view`, about to be shown in `level`, offered the space the level's root has.
    fn spaced(&self, level: Level, mut view: ViewInstance) -> ViewInstance {
        let space = match level {
            Level::Root => self.root_space.get("").copied(),
            Level::Detail => self.detail_space.get("").copied(),
        };
        if let Some(space) = space {
            view.set_space("", space);
        }
        view
    }

    /// Take in what the window tells of itself and of the person at it, which runs nothing
    /// there and then: where the keyboard is, whether an input method composes, the space a
    /// view has, and the undos and redos asked for, which the one who runs
    /// the instance carries out on the document ([`Self::take_history`]).
    fn window_told(&mut self, told: ToApp) {
        match told {
            // The notice in front takes back what it reported: the marks the person just moved
            // seen, where it was that notice, else the document's last change.
            ToApp::Undo => {
                let taken = std::mem::take(&mut self.seen_offer)
                    && self
                        .host()
                        .is_some_and(|host| host.runtime().undo_seen(&self.me_actor()));
                if !taken {
                    self.history.push(HistoryAsk::Undo);
                }
            }
            ToApp::Redo => self.history.push(HistoryAsk::Redo),
            ToApp::Focus { at } => self.focus = at,
            ToApp::Composing(composing) => self.composing = composing,
            ToApp::Held(held) => self.held = held,
            // Every write is sent as it is made; one the store refused is said, not kept.
            ToApp::SaveAgain => {}
            // What the person does next serves the call they signed for it: the writes it makes
            // are stamped with it, as the member it names, until the window ends it.
            ToApp::Serving { call, invoker } => {
                let call = if call.is_empty() {
                    None
                } else {
                    match serde_json::from_slice::<tilo_actions::remote::SignedCall>(&call) {
                        Ok(call) => Some(call),
                        Err(error) => {
                            tracing::error!(%error, "the window's signed call does not read");
                            return;
                        }
                    }
                };
                if let Some(host) = self.host() {
                    let store = host.store();
                    store.serve(
                        call.as_ref()
                            .map(|call| call.signed.statement().invocation.0.to_string()),
                    );
                    store.serve_invoker(invoker.filter(|_| call.is_some()));
                    store.serve_signed(call.map(Box::new));
                }
            }
            ToApp::Space {
                level,
                key,
                width,
                height,
                rem,
            } => self.offer_space(
                level,
                &key,
                f64::from(width),
                f64::from(height),
                f64::from(rem),
            ),
            other => tracing::error!(?other, "the window told of something it does not"),
        }
    }

    /// Carry out `message`, one thing the window told of the person at it, as a step of theirs,
    /// and send what followed it: what a press made, and what the window hears after a press.
    fn event(&mut self, message: ToApp) {
        let mut ran: Option<Ran> = None;
        let pressed = matches!(message, ToApp::Fire { .. });
        let outcome = {
            let _phase = tilo_resource::profiling::Phase::start("instance.event.carry_out");
            self.carry_out(message, &mut ran)
        };
        if let Err(words) = outcome {
            self.notice(words, ui::NoticeKind::Error);
            // What the controls show goes back to what they are bound to.
            for level in [Level::Root, Level::Detail] {
                if let Some(view) = self.level_mut(level) {
                    view.bound.clear();
                }
                self.send_bound(level);
            }
        }
        {
            let _phase = tilo_resource::profiling::Phase::start("instance.event.commit");
            self.commit_planned();
        }
        if let Some(ran) = ran {
            let _phase = tilo_resource::profiling::Phase::start("instance.event.after_action");
            self.after_action(ran);
        }
        self.follow_opens();
        if pressed {
            let _phase = tilo_resource::profiling::Phase::start("instance.event.pressed");
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
    /// takes the action it applied, what that returned and what it was run on, when it ran one.
    fn carry_out(&mut self, message: ToApp, ran: &mut Option<Ran>) -> Result<(), Words> {
        match message {
            ToApp::Back => {
                if self.window_history.is_some() {
                    self.close_history();
                } else {
                    self.close_detail();
                }
                Ok(())
            }
            ToApp::History { open } => {
                self.show_history(open);
                Ok(())
            }
            ToApp::Fire {
                level,
                key,
                event,
                value,
                gesture,
            } => self.fire(level, &key, &event, value.as_ref(), gesture.as_deref(), ran),
            edit @ (ToApp::Toggle { .. }
            | ToApp::Splice { .. }
            | ToApp::Edit { .. }
            | ToApp::EditRich { .. }) => self.edit_event(edit),
            // Only the system's confirm answers a call that asks the person: a window's answer to a
            // suggestion's card is none.
            ToApp::Resolve { .. } => Err(Words::phrase("tilolang-ask-needs-approval")),
            ToApp::Open { resource } => {
                self.show_now(&resource);
                Ok(())
            }
            ToApp::OpenReference { reference } => {
                self.open_reference(&reference);
                Ok(())
            }
            ToApp::Rows {
                level,
                list,
                start,
                end,
                open,
                menu,
            } => {
                self.rows_asked = self.rows_asked.saturating_add(1);
                let range = start as usize..end as usize;
                self.request_rows(level, &list, range, (open.as_deref(), menu.as_deref()));
                Ok(())
            }
            // A window draws no asking of its own: the shell's one asking view answers what was
            // asked, through the dispatch. And no page call waits for an answer yet: the window's
            // page runs nothing the app's actions ask of it.
            ToApp::Answer(_) | ToApp::PageAnswer { .. } => Ok(()),
            ToApp::PagePrompt { prompt } => {
                self.page_prompts.push(prompt);
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
            | ToApp::Held(_)
            | ToApp::SaveAgain
            | ToApp::Space { .. }
            | ToApp::Serving { .. }) => {
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
    /// in the document: what they opened, drawn, and the window told they ran, in the frame that
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

    /// Render the rows `range` of the list `list` of the view of `level` for the window, and the
    /// document of the row `open` — the row open into its item where it stands, which
    /// the window shows and no other row does.
    fn request_rows(
        &mut self,
        level: Level,
        list: &str,
        range: std::ops::Range<usize>,
        held: (Option<&str>, Option<&str>),
    ) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.level_mut(level) else {
            return;
        };
        let handle = Rc::clone(&view.view);
        let Some(rows) = view.lists.get_mut(list) else {
            return;
        };
        let requested = {
            let _phase = tilo_resource::profiling::Phase::start("instance.rows.request");
            rows.request(range, held)
        };
        let message = {
            let _phase = tilo_resource::profiling::Phase::start("instance.rows.message");
            rows_message(&host, &*names, &handle, level, list, requested)
        };
        self.out.push(message);
        // The rows rendered can hold lists of their own, and those scrolled away took theirs.
        let _phase = tilo_resource::profiling::Phase::start("instance.rows.track");
        self.track_lists(level);
        self.tell_spaces(level);
        self.send_bound(level);
    }

    /// Fire the event `name` of the node `key` of the view of `level`, carrying `value`, as one
    /// part of the person's act `gesture` (`None` for an event that is an act of its own); `ran`
    /// takes the action it applied, what that returned and what it was run on, when it ran one.
    fn fire(
        &mut self,
        level: Level,
        key: &str,
        name: &str,
        value: Option<&ui::Value>,
        gesture: Option<&str>,
        ran: &mut Option<Ran>,
    ) -> Result<(), Words> {
        // Counted whatever comes of it: the window waits for every press it sent.
        self.fired = self.fired.saturating_add(1);
        self.last_fire = self.fired;
        let Some(view) = self.view(level) else {
            return Ok(());
        };
        let value = value.and_then(wire::runtime_value);
        // What a drag from another view carried reaches this view by its published address and
        // nothing more (a chip dropped on a composer, a place's handle dropped on the canvas):
        // its type is read here, from the document this workspace holds or the reading its app
        // published, so the action the drop runs takes the resource it names. An address
        // neither knows yet keeps what it arrived as, and an action taking it as any resource
        // (`host.Resource`) still runs.
        let value = value.map(|value| self.resolved(value));
        let wanting = view
            .borrow()
            .wanting_arguments(key, name, value.as_ref())
            .map_err(|error| Words::Plain(error.to_string()))?;
        if let Some(aref) = wanting
            && self.open_form(level, key, Rc::clone(&view), aref)
        {
            return Ok(());
        }
        let action = view.borrow().event_action(key, name);
        let held = tilolang_runtime::Held {
            shift: self.held.shift,
            command: self.held.command,
            alt: self.held.alt,
        };
        // A check the project's host approved runs only for an invocation the workspace
        // dispatched, which its invoker can follow and stop: a press of it goes through the
        // dispatch, as the palette's does, rather than running here.
        let fired_action = view
            .borrow()
            .fired_action(key, name, value.as_ref(), held)
            .ok()
            .flatten();
        if let Some((address, argument)) = fired_action
            .as_ref()
            .and_then(|aref| self.dispatched_check(aref))
        {
            self.chosen.push_back((self.fired, address, Some(argument)));
            return Ok(());
        }
        if fired_action
            .as_ref()
            .is_some_and(|aref| self.handed_elsewhere(aref))
        {
            return Ok(());
        }
        // What the action is run on is read before it runs: a destructive one takes it away.
        let target = self
            .host()
            .zip(action.as_ref())
            .and_then(|(host, action)| target_title(&host, action));
        // The press runs as a call of its own, which its writes and the calls it makes continue.
        let press = self.host().zip(action.as_ref()).and_then(|(host, aref)| {
            press::begin(self.press_calls.as_deref(), &host, &self.app, aref)
        });
        let fired = view
            .borrow()
            .fire_held(key, name, value.as_ref(), gesture, held);
        self.pressed_ran(press, &fired);
        match fired {
            Ok(InvokeOutcome::Done(result)) => {
                *ran = action.map(|aref| Ran {
                    aref,
                    result,
                    target,
                });
                Ok(())
            }
            // It waits on a call it made through the dispatch, or on a person's yes to one: the
            // record it waits in is drawn by the shell's one asking view, and the answer runs it.
            Ok(InvokeOutcome::Waits { .. } | InvokeOutcome::Asked(_)) => Ok(()),
            Ok(InvokeOutcome::Denied) => Err(Words::phrase("tilolang-ask-denied")),
            Err(error) => Err(Words::Plain(error.to_string())),
        }
    }

    /// The address and the arguments a press of `aref` asks for when it runs a check the
    /// project's host approved (`uses check`): the app's `run-approved-check` on the resource it
    /// runs on, with the check's id. `None` for any other action, which runs here.
    fn dispatched_check(
        &self,
        aref: &crate::runtime::ActionRef,
    ) -> Option<(ActionAddress, String)> {
        let host = self.host()?;
        let app = host.find_in_packages(|package| {
            package
                .meta
                .actions
                .iter()
                .find(|action| action.id == aref.action)
                .filter(|action| crate::meta::runs_approved_check(action))
                .map(|_| package.meta.app.clone())
        })?;
        let (Some(Value::Ref(target)), Some(Value::Text(check_id))) =
            (aref.args.first(), aref.args.get(1))
        else {
            return None;
        };
        let address = ActionAddress::new(
            crate::publish::resource_id(&app, &target.target.id_string()),
            tilo_actions::remote::approved_checks::ACTION_ID,
        );
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({ "check_id": check_id })
        );
        Some((address, argument))
    }

    /// The press `press` began ran, as far as it runs now, to `outcome`: kept for the host.
    fn pressed_ran(
        &mut self,
        press: Option<(tilo_actions::InvocationId, ActionAddress)>,
        outcome: &Result<InvokeOutcome, crate::runtime::RuntimeError>,
    ) {
        let Some((invocation, address)) = press else {
            return;
        };
        if let Some(calls) = &self.press_calls {
            calls.end();
        }
        self.pressed.push(Pressed {
            invocation,
            address,
            outcome: PressOutcome::of(outcome),
        });
    }

    /// `value` with every reference in it naming its type: a resource a drag from another view
    /// carried reached this one by its published address, and its type is read from the
    /// workspace's document, else from the reading its app published ([`Runtime::resolve_ref`]).
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

    /// Show the form of `aref`, which the control `key` of the view of `level` ran and which
    /// still needs arguments, instead of running it. Whether it has one: what it needs is
    /// what a form can ask for.
    fn open_form(
        &mut self,
        level: Level,
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
            .find(|package| {
                package
                    .meta
                    .actions
                    .iter()
                    .any(|action| action.id == aref.action)
            })
            .map(|package| package.meta.clone())
        else {
            return false;
        };
        self.open_form_with(Some((level, key.to_owned())), view, aref, meta)
    }

    /// Show the form of `aref`, an action of `meta` waiting for arguments a form asks for,
    /// where `from` names the control that ran it, or on its own.
    fn open_form_with(
        &mut self,
        from: Option<(Level, String)>,
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
    /// when it still needs arguments a form asks for: the action as the document it is bound to
    /// gives it. Whether it opened one.
    fn open_chosen_form(&mut self, address: &ActionAddress) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let Some(action) = host.published_action(&address.resource, &address.action) else {
            return false;
        };
        let Some(meta) = host
            .packages()
            .into_iter()
            .find(|package| {
                crate::publish::app_of(&address.resource) == Some(package.meta.app.as_str())
            })
            .map(|package| package.meta.clone())
        else {
            return false;
        };
        // What the address names the action of, when the action's first parameter is its
        // target: the resource the descriptor was published on.
        let mut args = Vec::new();
        if let Some(target) = meta.target_of(&action.params) {
            // The address names the resource as the API publishes it (`tl:<app>:<id>`); the
            // runtime holds it by its id.
            let id = address
                .resource
                .strip_prefix(&crate::publish::resource_id(&meta.app, ""))
                .unwrap_or(&address.resource);
            let Some(target) = ResourceRef::parse_id(id, target.id.clone()) else {
                return false;
            };
            args.push(Value::resource(&target));
        }
        // What is still to ask is what the action has no default for, as the label's "…" and
        // every form read it: when the address's target leaves nothing, the action runs as it
        // is, as a control that runs it runs it.
        if action.unbound(args.len()).next().is_none() {
            return false;
        }
        let Some(view) = self.view(Level::Root) else {
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
    fn form_submit(&mut self, id: u64, ran: &mut Option<Ran>) -> Result<(), Words> {
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
        // A check the project's host approved goes through the dispatch, as a press of it does.
        if let Some((address, argument)) = self.dispatched_check(&aref) {
            if let Some(pending) = self.form.take() {
                pending.forget();
            }
            self.out.push(ToUi::Form(None));
            self.chosen.push_back((self.fired, address, Some(argument)));
            return Ok(());
        }
        if self.handed_elsewhere(&aref) {
            if let Some(pending) = self.form.take() {
                pending.forget();
            }
            self.out.push(ToUi::Form(None));
            return Ok(());
        }
        let Some(pending) = self.form.as_mut().filter(|open| open.id == id) else {
            return Ok(());
        };
        let target = target_title(&host, &aref);
        let press = press::begin(self.press_calls.as_deref(), &host, &self.app, &aref);
        let outcome = pending.view.borrow().invoke(&aref);
        if let Some((invocation, address)) = press {
            if let Some(calls) = &self.press_calls {
                calls.end();
            }
            self.pressed.push(Pressed {
                invocation,
                address,
                outcome: PressOutcome::of(&outcome),
            });
        }
        match outcome {
            Ok(InvokeOutcome::Done(result)) => {
                pending.forget();
                self.form = None;
                self.out.push(ToUi::Form(None));
                *ran = Some(Ran {
                    aref,
                    result,
                    target,
                });
                Ok(())
            }
            // It waits on a call it made through the dispatch, or on a person's yes to one: the
            // record it waits in is drawn by the shell's one asking view, and the answer runs it.
            Ok(InvokeOutcome::Waits { .. } | InvokeOutcome::Asked(_)) => {
                pending.forget();
                self.form = None;
                self.out.push(ToUi::Form(None));
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
                level,
                target,
                key,
                checked,
            } => self.edit(level, &target, &key, Value::Bool(checked), None),
            ToApp::Splice {
                level,
                target,
                key,
                seq,
                at,
                delete,
                insert,
            } => self.splice(level, &target, &key, seq, (at, delete), &insert),
            ToApp::Edit {
                level,
                target,
                key,
                text,
            } => self.edit(level, &target, &key, Value::Text(text), None),
            ToApp::EditRich {
                level,
                target,
                key,
                rich,
                gesture,
            } => {
                let rich = Value::Rich(wire::runtime_rich(&rich));
                self.edit(level, &target, &key, rich, gesture.as_deref())
            }
            _ => Ok(()),
        }
    }

    /// Where an edit the window made in the control `key` of the view of `level` showing
    /// `target` applies.
    fn edited(&self, level: Level, target: &ui::ResourceRef, key: &str) -> Result<Place, Words> {
        if let Some(shown) = self.level(level) {
            // The level's own view, showing the item the edit was made in, or drawing the
            // control itself: a row of its list open into its item where it stands holds the
            // item's page, whose controls are the view's own and bound to the item.
            if wire::resource(&shown.target) == *target || shown.view.borrow().find(key).is_some() {
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

    /// Apply a splice of the bound text control `key` of the view of `target` in `level`:
    /// `delete` bytes at `at` replaced by `insert`, against the text it shows.
    fn splice(
        &mut self,
        level: Level,
        target: &ui::ResourceRef,
        key: &str,
        seq: u64,
        (at, delete): (u32, u32),
        insert: &str,
    ) -> Result<(), Words> {
        let place = self.edited(level, target, key)?;
        if place.shown()
            && let Some(level) = self.level_mut(level)
        {
            let acked = level.acked.entry(key.to_owned()).or_default();
            *acked = (*acked).max(seq);
        }
        let current = match place.view().borrow().bound_value(key) {
            Some(Value::Text(text)) => text,
            _ => String::new(),
        };
        let text = spliced(&current, at as usize, delete as usize, insert);
        self.edit_at(level, target, place, key, Value::Text(text), None)
    }

    /// Apply an edit of the bound control `key` of the view of `target` in `level`.
    fn edit(
        &mut self,
        level: Level,
        target: &ui::ResourceRef,
        key: &str,
        value: Value,
        gesture: Option<&str>,
    ) -> Result<(), Words> {
        let place = self.edited(level, target, key)?;
        self.edit_at(level, target, place, key, value, gesture)
    }

    /// Apply the edit of the bound control `key` made in the view of `target` in `level`, where
    /// the window's page draws `target` or another item: the page shows the item while the edit
    /// lands, and the window shows again the item it showed, with what the edit made.
    fn edit_at(
        &mut self,
        level: Level,
        target: &ui::ResourceRef,
        place: Place,
        key: &str,
        value: Value,
        gesture: Option<&str>,
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
            let outcome = Self::edit_view(&view.borrow(), key, value, gesture);
            if let Some(before) = beside
                && let Err(error) = view.borrow().show(&before)
            {
                tracing::warn!(%error, "the item the window showed did not show again");
            }
            // The page is what the window showed, drawn again whole: the frame that takes the
            // item it showed and the one it showed the edit in is one.
            if let Some(shown) = self.level_mut(level) {
                shown.sent = None;
                shown.bound.clear();
            }
            outcome?;
            self.send_open(level, None);
            return Ok(());
        }
        let view = place.into_view();
        Self::edit_view(&view.borrow(), key, value, gesture)?;
        if shown {
            self.shown_by_window(level, key);
        }
        Ok(())
    }

    fn edit_view(
        view: &ViewInstance,
        key: &str,
        value: Value,
        gesture: Option<&str>,
    ) -> Result<(), Words> {
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
        match view.edit_in(key, &value, gesture) {
            Ok(EditOutcome::Applied) => Ok(()),
            Ok(EditOutcome::NeedsApproval) => Err(Words::phrase("tilolang-ask-needs-approval")),
            Ok(EditOutcome::Denied) => Err(Words::phrase("tilolang-ask-denied")),
            Err(error) => Err(Self::refusal(&error)),
        }
    }

    /// What the window says about an edit that did not apply: the reason in the person's own
    /// words when the app declined for one they can act on, else the fault as it is.
    fn refusal(error: &tilolang_runtime::RuntimeError) -> Words {
        if let tilolang_runtime::RuntimeError::Store(
            tilolang_runtime::store::StoreError::Declined(reason),
        ) = error
        {
            let (id, args) = reason.words();
            return Words::Phrase { id, args };
        }
        Words::Plain(error.to_string())
    }

    /// What follows an action a person ran from a control: a destructive one offers Undo,
    /// naming what it was run on; what a primary one made is shown.
    fn after_action(&mut self, ran: Ran) {
        let Ran {
            aref,
            result,
            target,
        } = ran;
        let Some(host) = self.host() else {
            return;
        };
        // The action the control ran, when its app declares it: the platform's own (`file.choose`,
        // which runs the action a file fills and answers what that made) is nobody's and reads as
        // an action with no role.
        let action = host.find_in_packages(|package| {
            package
                .meta
                .actions
                .iter()
                .find(|action| action.id == aref.action)
                .cloned()
        });
        let role = action.as_ref().and_then(|action| action.role.as_deref());
        match (role, &action) {
            (Some("destructive"), Some(action)) => {
                let label = crate::runtime::platform::label_of(action);
                let words = match target {
                    Some(title) => Words::Phrase {
                        id: "tilolang-did".to_owned(),
                        args: vec![("action".to_owned(), label), ("title".to_owned(), title)],
                    },
                    None => Words::Plain(label),
                };
                // A destructive action is undone rather than confirmed.
                self.seen_offer = false;
                self.out.push(ToUi::Notice {
                    words,
                    kind: ui::NoticeKind::Info,
                    undo: true,
                });
            }
            (Some("primary"), _) => {
                // What a primary action made opens beside the list, as the person asked for it.
                if let Some(made) = made_resource(&result) {
                    self.pressed_opens.push((self.last_fire, made.id_string()));
                }
            }
            _ => {}
        }
        // What any other action a person ran from a control made shows where the window shows
        // what it made, as a search hit of it does: a page imported into the notebook stands
        // beside its tree. The app's own view decides; one with no place for it leaves the
        // window as it was.
        if role != Some("primary")
            && let Some(made) = made_resource(&result)
        {
            self.pressed_shows.push((self.last_fire, made.id_string()));
        }
    }

    // ----- frames --------------------------------------------------------------------------

    /// Something other than this window changed what the store holds: what it read is read
    /// again by the next frame.
    pub fn store_moved(&mut self) {
        self.store.moved();
        self.moved = true;
    }

    /// The records `docs` moved on, written by another device: what this window read of those
    /// records is read again, and what it read of the rest stays, so a workspace changed as a
    /// whole is read again the records that changed, not all of it.
    pub fn records_moved(&mut self, docs: &BTreeSet<tilolang_runtime::ids::DocId>) {
        self.store.moved_docs(docs);
        self.moved = true;
    }

    /// The first half of a frame: open the host and the root, stage a scenario, and send the
    /// window what changed.
    pub fn sync(&mut self) {
        self.frame();
    }

    fn frame(&mut self) {
        let Some(host) = self.host_for() else {
            return;
        };
        let host = self.follow_updates(host);
        if !self.started {
            self.started = true;
            self.loading = false;
            if !self.rootless {
                let _phase = tilo_resource::profiling::Phase::start("tl.view.open_root");
                self.open_root(&host);
            }
        }
        // Each trigger run is the workspace's once, on the device whose turn it is.
        if self.mode == Mode::Live
            && let Some(now) = self.now()
            && host.run_triggers(now) > 0
        {
            self.moved = true;
        }
        // A secret shown for its time is hidden again, in every window showing it.
        if let Some(now) = self.now()
            && crate::page::lapse_shown(now)
        {
            self.resend(Level::Root);
            self.resend(Level::Detail);
        }
        if self.staged.is_some() {
            self.run_staged(&host);
        }
        if std::mem::take(&mut self.moved) {
            self.settle_due = true;
            self.facts_stale |= self.facts.is_some();
            if let Some(publication) = &self.publication {
                publication.update();
            }
            self.send_bound(Level::Root);
            self.send_bound(Level::Detail);
        } else if let Some(publication) = self.publication.as_ref().filter(|p| p.pending()) {
            publication.update();
        }
        self.poll_views();
        self.poll_facts();
        self.flush();
    }

    /// Whether work is still under way off the frame that changes what the window shows: records
    /// the publication is reading.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.publication
            .as_ref()
            .is_some_and(|publication| publication.pending())
    }

    /// Whether a frame has work the window has not seen: a change no sync has sent it yet
    /// (`moved`), a write the declared effects owe ([`Self::settle_owed`]), or work under way off the
    /// frame ([`Self::pending`]). A window that owes one syncs before it rests.
    #[must_use]
    pub fn owes_a_frame(&self) -> bool {
        self.moved || self.settle_owed() || self.pending()
    }

    /// Whether the declared effects owe a settlement a frame would take in: only a live window
    /// settles it, only once it reads the document, and only while an effect waits (a
    /// window that commits after every frame, and committed nothing, rests).
    fn settle_owed(&self) -> bool {
        self.settle_due && self.mode == Mode::Live
    }

    /// Wait for the work under way off the frame and take it in, so that what the window shows
    /// next is read from a publication that covers the store: for an embedder with no frame to
    /// take it in on. Whether what the window shows may have moved.
    pub fn settle_pending(&mut self) -> bool {
        let published = self
            .publication
            .as_ref()
            .is_some_and(|publication| publication.settle());
        self.moved |= published;
        published
    }

    /// The second half of a frame: what the person did and what the API answered, run in the
    /// runtime, and what changed sent. Whether anything changed.
    pub fn update(&mut self) -> bool {
        let received = {
            let _phase = tilo_resource::profiling::Phase::start("instance.receive");
            self.receive()
        };
        let opened = self.take_open_requests();
        let pulled = self.poll_origins();
        let settled = self.settle_effects();
        // The person's environment — their language, their formats, what the member list calls
        // someone — changed: the boundaries that show what only the window knows run again, and
        // no other.
        let person = std::mem::take(&mut self.person_changed);
        if person {
            for view in [self.root.as_ref(), self.detail.as_ref()]
                .into_iter()
                .flatten()
            {
                view.view.borrow_mut().person_moved();
            }
            self.facts_stale |= self.facts.is_some();
        }
        // Who is at a resource shows wherever a view reads its lenses.
        let lenses = std::mem::take(&mut self.lenses_changed);
        let members = self.follow_members();
        // What the workspace's owners let members post there, and what a limit refused: the
        // views that read `workspace.content` show a refusal as it stands and as it goes.
        let content = self.host().is_some_and(|host| host.follow_content());
        // The time a view read moved on: the frame its embedder woke for it.
        let timed = self.wakes_in().is_some_and(|wait| wait.is_zero());
        let changed = received
            || opened
            || pulled
            || settled
            || lenses
            || person
            || members
            || content
            || timed;
        // A view runs where something reached it: what a change told its store, a state it
        // read, the time it waits for ([`crate::runtime::ViewInstance::woken`]).
        let changed = {
            let _phase = tilo_resource::profiling::Phase::start("instance.views");
            self.poll_views()
        } || changed;
        let changed = {
            let _phase = tilo_resource::profiling::Phase::start("instance.facts");
            self.poll_facts()
        } || changed;
        if person {
            // A name, a word and how a day or a time of day reads go into a node's value when it
            // is sent: every tree is sent again.
            for level in [Level::Root, Level::Detail] {
                self.resend(level);
            }
        }
        // The presses ran and what they opened is sent: what the person typed after them can
        // go to the field focused now.
        self.tell_took();
        let _phase = tilo_resource::profiling::Phase::start("instance.flush");
        self.flush();
        changed
    }

    /// Give the runtime the workspace's members as the workspace's policy has them now, when it
    /// changed ([`Host::follow_members`]): the host reads the access this instance gives it and
    /// its members' resource, which `names` reads the people this window shows from.
    fn follow_members(&mut self) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let changed = host.follow_members();
        if changed {
            self.member_names_moved = true;
        }
        changed
    }

    /// What the workspace's members' resource calls each member, when it named a different list
    /// since the last time: the window that presents this app has no connection of its own, so
    /// what a presence mark of its own draws reads its names here (the host's members read).
    pub fn take_member_names(&mut self) -> Option<Vec<(MemberId, String)>> {
        if !std::mem::take(&mut self.member_names_moved) {
            return None;
        }
        let rows = self.host()?.member_rows();
        Some(
            rows.values()
                .filter(|row| !row.removed && !row.name.trim().is_empty())
                .map(|row| (row.id.clone(), row.name.clone()))
                .collect(),
        )
    }

    /// The person at the window, as the runtime names them.
    fn me_actor(&self) -> Actor {
        Actor::Member(MemberId::Person(self.me.clone()))
    }

    /// Send the window the tree of `level` again as patches, read anew.
    fn resend(&mut self, level: Level) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.level_mut(level) else {
            return;
        };
        let tree = window_tree(&host, &*names, &view.view, view.with.is_some());
        let mut patches = Vec::new();
        if let Some(sent) = &view.sent {
            wire::diff(sent, &tree, &mut patches);
        }
        view.sent = Some(tree);
        if !patches.is_empty() {
            self.out.push(ToUi::Patch { level, patches });
        }
    }

    /// What the app's actions said in passing since the window was last sent
    /// (`notice(...)`): each is the window's bar's notice, shown there for a few seconds.
    fn take_said(&mut self) {
        let Some(host) = self.host.borrow().clone() else {
            return;
        };
        for said in host.runtime().take_said() {
            self.out.push(ToUi::Notice {
                words: Words::Plain(said.text),
                kind: match said.kind {
                    crate::runtime::notice::Kind::Info => ui::NoticeKind::Info,
                    crate::runtime::notice::Kind::Success => ui::NoticeKind::Success,
                    crate::runtime::notice::Kind::Warning => ui::NoticeKind::Warning,
                    crate::runtime::notice::Kind::Error => ui::NoticeKind::Error,
                },
                undo: said.undo,
            });
        }
    }

    /// Send what waits as this frame's batch.
    pub fn flush(&mut self) {
        self.take_said();
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
            Ok(batch) => match self.transport.borrow_mut().send(batch) {
                // A window that closed is shown nothing more.
                Ok(()) | Err(tilo_render_protocol::WireError::Closed) => {}
                Err(error) => tracing::warn!(%error, "the window's connection failed"),
            },
            Err(error) => tracing::error!(%error, "a frame's batch did not encode"),
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

/// The other packages of the workspace whose documents the package `app` holds references to.
fn used_packages(host: &Host, app: &str) -> Vec<Rc<Package>> {
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

/// The type `ty` (an id), as the package that declares it declares it.
#[must_use]
pub fn type_of(host: &Host, ty: &str) -> Option<TypeMeta> {
    host.find_in_packages(|package| {
        package
            .meta
            .types
            .iter()
            .find(|declared| declared.id == ty)
            .cloned()
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

pub use crate::runtime::platform::sentence;

/// The title of a resource, as every surface that names it shows it: its first text field's
/// first line; else, for a package's root, the package's title; else what its type declares for
/// it while untitled; else its type's name.
#[must_use]
pub fn title_of(host: &Host, resource: &ResourceRef) -> String {
    host.runtime().title(resource).unwrap_or_default()
}

/// The title of what `aref` is run on — the item of a list (`remove(s, b)`), or the resource —
/// when it has one. Read before the action runs: a destructive one takes its target away.
fn target_title(host: &Host, aref: &ActionRef) -> Option<String> {
    let target = aref.args.iter().rev().find_map(|arg| match arg {
        Value::Ref(target) => Some(ResourceRef::clone(&target.target)),
        _ => None,
    })?;
    let title = title_of(host, &target);
    (!title.is_empty()).then_some(title)
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
mod tests;

/// The resource an action returned, as itself or inside an optional: an action that may make
/// nothing (`-> Option<Book>`) still opens what it made when it made one.
fn made_resource(value: &Value) -> Option<&ResourceRef> {
    match value {
        Value::Ref(made) => Some(&made.target),
        Value::Opt(Some(held)) => made_resource(held),
        _ => None,
    }
}
