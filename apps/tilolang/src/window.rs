//! The window of a tilolang app: its views rendered onto the native shell.
//!
//! The window runs the package's root view, and the view of the resource `open(r)` (or a row of
//! a list) went to. They are consecutive windows of one package in the lane, and they render as
//! the columns of one shell ([`shell`]): each view builds its own (`ui.NavShell`; `std` builds
//! one for every frame), and the window shows the root's sidebar and overview beside the
//! content and header of the window after it. On a narrow screen the columns become push
//! navigation with Back, by the shell's rules. Everything in the shell is the views' own,
//! built through the one adapter: the window adds no frame, header or list of its own.
//!
//! What holds for every window is the platform's, and so the window's: the overview's
//! selection is the window shown after it; the lenses on each resource are what presence
//! reports; a destructive action a person runs offers Undo; a primary action a person runs
//! shows what it made.
//!
//! Each frame the window
//!
//! 1. brings the runtime's mirror of the room record up to date with the record,
//! 2. re-runs a view if what it read changed and brings the shell up to date with its tree,
//! 3. takes what the person did and runs it in the runtime, which plans the change, and
//! 4. hands what the runtime planned to the door, as an invocation of the host's `commit`
//!    action, so the change is attributed, undoable and replayed by every peer.

mod describe;
pub(crate) mod pane;
pub(crate) mod rows;
pub(crate) mod shell;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value as Json;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{COMMIT, HOST_RESOURCE};
use tilolang_tilo::runtime::handles::Retained;
use tilolang_tilo::runtime::{
    ActionInfo, ActionRef, BindTarget, Decision, EditOutcome, InvokeOutcome, OutsideSink, Patch,
    Principal, Resolution, ResourceRef, Suggestion, UiNode, Value, ViewEvent, ViewInstance,
};
use tilolang_tilo::sink::LogSink;
use tilolang_tilo::upgrade::Upgrade;
use tilolang_tilo::{HostEffect, OpenEffect};
use ui::framework::{ResourceSurface, RunMode};
use ui::tilo_actions::{ActionAddress, ControlInvocation, InvocationId};
use ui::tokens::{MotionSource, Rhythm, TextRole};
use ui::widget::arrange::{Fit, Row, Slot, Stack};
use ui::widget::controls::ControlResult;
use ui::widget::toast::ToastKind;
use ui::widget::{
    Absent, AppModel, Button, ButtonRole, DescribedList, Element, ModelPath, NavPane, NavShell,
    Text, Widget,
};

use crate::i18n::{t, t_args};
use crate::names::Names;
use crate::reconcile::{EventSink, Reconciler, UiEvent};
use crate::resource::{Live, Room, RoomState, caller_of};
use pane::Pane;
use rows::{HostRows, Registry};
use shell::DETAIL;
use ui::tilo_resource::profiling::Phase;

/// An answer to the Ask bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Allow,
    Decline,
    Always,
}

/// Commits waiting to be handed to the door, each tagged with the invocation of the door that
/// made it, when one did: the framework applies it for the invoker the door admitted.
type Outgoing = Rc<RefCell<VecDeque<(ControlInvocation, Option<InvocationId>)>>>;

/// The retained event the composed shell's Back carries: the window answers it itself.
const BACK_EVENT: Retained = Retained(u64::MAX);

/// Hand everything the runtime committed under a ticket of `first` or later to the door.
/// Returns the tickets handed over.
fn queue_commits(
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

/// A sink the window holds as a trait object, handed to the runtime.
struct BoxedSink(Box<dyn OutsideSink>);

impl OutsideSink for BoxedSink {
    fn deliver(&mut self, capability: &str, payload: &Json) -> Result<(), String> {
        self.0.deliver(capability, payload)
    }
}

/// The two vocabularies the window answers on the door: what an agent asks of the host, and
/// showing what a search hit names.
fn door_endpoints(
    host: &Rc<RefCell<Option<Host>>>,
    door_work: &Rc<Cell<bool>>,
    outgoing: &Outgoing,
    open_requests: &Rc<RefCell<Vec<String>>>,
) -> (Element, Element) {
    let answering = (Rc::clone(host), Rc::clone(door_work), Rc::clone(outgoing));
    let endpoint = ui::framework::EffectEndpoint::new(
        move |effect: HostEffect, caller: &ui::framework::EffectCaller| {
            let host = answering
                .0
                .borrow()
                .clone()
                .ok_or_else(|| "the tilolang host has not started".to_owned())?;
            let who = caller.invoker.as_ref().map(caller_of);
            let first = host.next_ticket();
            let answered = host.answer(effect, who.as_ref());
            queue_commits(&host, &answering.2, first, Some(caller.invocation));
            answering.1.set(true);
            answered
        },
    );
    let opening = Rc::clone(open_requests);
    let open_endpoint = ui::framework::EffectEndpoint::new(
        move |effect: OpenEffect, _: &ui::framework::EffectCaller| {
            let OpenEffect::Open { anchor } = effect;
            opening.borrow_mut().push(anchor);
            Ok(ui::tilo_actions::ActionOutput::default())
        },
    );
    (Box::new(endpoint), Box::new(open_endpoint))
}

/// What moves a column the person's last input brings to the front: a click or a tap slides it
/// in; a key, an agent's action through the door, or nothing the person just did lands it.
fn by_input() -> MotionSource {
    ui::motion::recent_input(ui::time::now()).unwrap_or(MotionSource::Action)
}

/// The virtual lists of a view's tree, by key, with their keys from the root.
fn virtual_lists(tree: &UiNode) -> Vec<(String, Vec<String>)> {
    fn walk(node: &UiNode, path: &mut Vec<String>, out: &mut Vec<(String, Vec<String>)>) {
        path.push(node.key.clone());
        if tilolang_tilo::runtime::view::is_virtual(&node.kind) {
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

fn slot(element: Element, fit: Fit) -> Slot {
    let mut slot = Slot::new(element);
    slot.fit = fit;
    slot
}

/// One window of the lane: a resource shown through a view.
struct Lane {
    target: ResourceRef,
    /// The view `open(r, view: v)` named, when it named one; the default view of the type
    /// otherwise.
    with: Option<String>,
    /// The view, shared with the rows of its virtual lists, which render what they show.
    view: Rc<RefCell<ViewInstance>>,
}

/// What a person's action, run from a control, is waiting for the door to commit: the notice of
/// a destructive one, which offers Undo once it is committed.
#[derive(Default)]
struct Pending {
    undo: HashMap<u64, String>,
}

/// The window. See the module docs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent frame flags of one window: started, dirty, staged, moved"
)]
pub struct Window {
    app: String,
    me: String,
    mode: RunMode,
    host: Rc<RefCell<Option<Host>>>,
    live: Option<Live>,
    names: Names,
    names_changed: Cell<bool>,
    /// The lenses on each resource of the package, as presence last reported them.
    lenses: HashMap<String, Vec<(Principal, bool)>>,
    lenses_changed: bool,
    root: Option<Lane>,
    /// The window `open(r)` or a row went to: the next column, or the content in place of the
    /// root's when the root has no overview.
    detail: Option<Lane>,
    /// Builds the composed shell and reconciles it, reporting what its controls do.
    reconciler: Rc<RefCell<Reconciler>>,
    events: EventSink,
    /// The composed shell last built.
    built: Option<UiNode>,
    /// The rows of the views' virtual lists.
    rows: Registry,
    /// The shell, the Ask bar and the door's endpoints.
    body: Element,
    structure_dirty: bool,
    outgoing: Outgoing,
    in_flight: VecDeque<u64>,
    door_work: Rc<Cell<bool>>,
    open_requests: Rc<RefCell<Vec<String>>>,
    answers: Rc<RefCell<Vec<Answer>>>,
    ask: Option<Suggestion>,
    settle_due: bool,
    sink: RefCell<Option<Box<dyn OutsideSink>>>,
    device: Option<String>,
    /// Where the indexes of the rooms this window shows are kept: `None` keeps them in memory.
    indexes: Option<std::path::PathBuf>,
    /// The room this window shows, which names where its indexes are kept: had from the launch,
    /// before the room's record.
    room: Option<String>,
    /// The room's record, while it is read off the frame and the window shows what the indexes
    /// hold ([`Self::opening`]).
    opening: Option<ui::record::RecordOpening>,
    /// Whether the window shows the platform's loading state, until the indexes are read from.
    loading: bool,
    pending: Pending,
    undo: ui::undo::UndoAccess,
    /// The scenario a capture stages, until the host runs it.
    staged: Option<String>,
    started: bool,
    moved: bool,
    /// The later build of the package this window runs (`crate::updates`), 0 for the installed
    /// one.
    generation: u64,
    /// Whether this window is woken when a later build arrives.
    follows: bool,
    /// A later build this window took and has not drawn yet: its first frame says so.
    undrawn: Option<Undrawn>,
}

/// A later build a window took, until its first frame.
struct Undrawn {
    generation: u64,
    read_at: std::time::Instant,
}

/// The other packages of the room whose records the package `app` holds references to.
fn used_packages(host: &Host, app: &str) -> Vec<tilolang_tilo::host::Package> {
    let Some(own) = host.package(app) else {
        return Vec::new();
    };
    host.packages()
        .into_iter()
        .filter(|other| other.meta.app != app)
        .filter(|other| {
            let prefix = format!("{}.", other.meta.app);
            own.meta
                .types
                .iter()
                .flat_map(|ty| &ty.fields)
                .any(|field| field.ty.contains(&prefix))
        })
        .collect()
}

impl Window {
    /// The window of the package `app` for the participant `me`.
    #[must_use]
    pub fn new(app: String, me: String, mode: RunMode) -> Self {
        let host: Rc<RefCell<Option<Host>>> = Rc::default();
        let open_requests: Rc<RefCell<Vec<String>>> = Rc::default();
        let door_work: Rc<Cell<bool>> = Rc::default();
        let outgoing: Outgoing = Rc::default();
        let (endpoint, open_endpoint) =
            door_endpoints(&host, &door_work, &outgoing, &open_requests);
        let body = Stack::stack()
            .fill_width()
            .fill_height()
            .gap(Rhythm::Tight)
            .push_slot(slot(
                Box::new(Pane::new(Box::new(NavShell::new()))),
                Fit::fill(1.0),
            ))
            .push_slot(slot(Box::new(Absent::new()), Fit::hug()))
            .push_slot(slot(endpoint, Fit::hug()))
            .push_slot(slot(open_endpoint, Fit::hug()));
        let names = Names::new(&me);
        let events: EventSink = Rc::default();
        let rows: Registry = Rc::default();
        let reconciler = {
            let field_names = Self::field_names(&host);
            let cases = Self::case_lookup(&host);
            let people = {
                let names = names.clone();
                Rc::new(move |id: &str| names.label(id)) as crate::reconcile::People
            };
            // Weakly: the rows' own readers share this reconciler's lookups, and a strong
            // hold would keep the rows, their views and the room's indexes alive after the
            // window closes.
            let registry = Rc::downgrade(&rows);
            let rows_of: crate::adapter::RowsOf = Rc::new(move |key: &str| {
                registry
                    .upgrade()?
                    .borrow()
                    .get(key)
                    .map(|rows| Rc::clone(rows) as ui::component::SharedRows)
            });
            Reconciler::new(Rc::clone(&events), field_names)
                .with_lookups(cases, people)
                .with_rows(rows_of)
        };
        Self {
            names,
            app,
            me,
            mode,
            host,
            live: None,
            names_changed: Cell::new(false),
            lenses: HashMap::new(),
            lenses_changed: false,
            root: None,
            detail: None,
            reconciler: Rc::new(RefCell::new(reconciler)),
            events,
            built: None,
            rows,
            body: Box::new(body),
            structure_dirty: true,
            outgoing,
            in_flight: VecDeque::new(),
            door_work,
            open_requests,
            answers: Rc::default(),
            ask: None,
            settle_due: false,
            sink: RefCell::new(Some(Box::new(LogSink::dev()))),
            device: None,
            indexes: None,
            room: None,
            opening: None,
            loading: false,
            pending: Pending::default(),
            undo: ui::undo::UndoAccess::new(),
            staged: None,
            started: false,
            moved: false,
            generation: 0,
            follows: false,
            undrawn: None,
        }
    }

    /// The anchors search hits and agents asked the window to show, until it shows them.
    #[must_use]
    pub fn open_requests(&self) -> Rc<RefCell<Vec<String>>> {
        Rc::clone(&self.open_requests)
    }

    /// This window on the device `device`: the one that carries out the outside effects its
    /// room queues.
    #[must_use]
    pub fn on_device(mut self, device: &str) -> Self {
        self.device = Some(device.to_owned());
        self
    }

    /// This window keeping the indexes of the rooms it shows under `dir`, so the room opens
    /// next time over them rather than reading its whole record.
    #[must_use]
    pub fn keep_indexes_in(mut self, dir: std::path::PathBuf) -> Self {
        self.indexes = Some(dir);
        self
    }

    /// This window in the room `room`, as its launch names it: where its indexes are kept is
    /// known before the room's record is.
    #[must_use]
    pub fn in_room(mut self, room: &str) -> Self {
        self.room = Some(room.to_owned());
        self
    }

    /// This window shown before the room's record, which `record` reads off the frame: it opens
    /// over the room's indexes, and a write, or a field no index holds, waits for the record
    /// ([`tilolang_tilo::host::Host::set_record_loader`]). Its view takes it over once the
    /// record is had ([`Self::taken_over`]).
    #[must_use]
    pub fn opening(mut self, record: ui::record::RecordOpening) -> Self {
        self.opening = Some(record);
        self
    }

    /// This window, shown before the room's record, taken over by the view of the record: it
    /// reads the record from now on, through the framework as every window does.
    #[must_use]
    pub fn taken_over(mut self) -> Self {
        self.opening = None;
        self
    }

    /// Where the indexes of this window's room are kept, when they are kept on disk.
    fn index_dir(&self) -> Option<std::path::PathBuf> {
        if self.mode != RunMode::Live {
            // A past version is read once and kept nowhere, and a capture shows its scenario
            // on a profile of its own, thrown away with it: their indexes are built in memory,
            // at once.
            return None;
        }
        Some(tilolang_tilo::index::room_dir(
            self.indexes.as_deref()?,
            self.room.as_deref()?,
        ))
    }

    /// This window sending the outside effects it settles to `sink` rather than the development
    /// log.
    #[must_use]
    pub fn with_sink(self, sink: impl OutsideSink + 'static) -> Self {
        *self.sink.borrow_mut() = Some(Box::new(sink));
        self
    }

    // ----- the host ------------------------------------------------------------------------

    /// The host over the room record `doc`, built with the package the first time; without the
    /// record (while it is read off the frame), over the room's indexes.
    fn host_for(&mut self, doc: Option<&ui::automerge::AutoCommit>) -> Option<Host> {
        if let Some(host) = self.host.borrow().as_ref() {
            // An index built off the frame changes what every view reads, as a change does.
            let synced = doc.is_some_and(|doc| host.sync_from(doc));
            if synced | host.poll_index() {
                self.moved = true;
            }
            return Some(host.clone());
        }
        let index_dir = self.index_dir();
        let loader = match (doc, &self.opening, &index_dir) {
            (Some(_), _, _) => None,
            (None, Some(record), Some(_)) => Some(record.clone()),
            // Without the record, a window opens over the indexes it keeps or not at all.
            (None, _, _) => return None,
        };
        let opening = tilolang_tilo::host::Opening {
            record: doc,
            index_dir,
            device: self.device.as_deref(),
        };
        let built = Host::open(
            opening,
            Principal::new(self.me.clone()),
            |_: &Principal, _: &ActionInfo| {
                // What the person may do in the room is decided where the change is committed:
                // the door refuses a viewer's `commit`. Code authorship is the runtime's.
                Decision::Allow
            },
        );
        let host = match built {
            Ok(host) => host,
            Err(error) => {
                tracing::error!(%error, "the tilolang runtime could not be set up");
                return None;
            }
        };
        if let Some(record) = loader {
            // A write, or a field no index holds, waits for the record and lands on it.
            host.set_record_loader(Box::new(move || record.document()));
        }
        host.runtime().set_name_host(self.names.clone());
        // `now` is the toolkit's clock, as every time the window shows is: a capture pins it, so
        // a scenario's messages are stamped with the capture's time.
        host.set_clock(|| {
            ui::clock::local_now()
                .with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        });
        // The platform's phrases, which the standard library's frames say.
        host.runtime().set_phrases(|id, args| {
            let args: Vec<(&str, ui::i18n::ArgValue)> = args
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str().into()))
                .collect();
            t_args(id, &args)
        });
        if let Some(sink) = self.sink.borrow_mut().take() {
            host.runtime().set_sink(BoxedSink(sink));
        }
        self.generation = crate::app::load(&host);
        if crate::updates::followed() && self.mode != RunMode::Past && !self.follows {
            self.follows = true;
            crate::updates::subscribe(ui::wake::WakeHandle::new());
        }
        if self.mode != RunMode::Past {
            // What is published and found of the room is read off the frame the first time;
            // the window takes it in when it is ready.
            let wake = ui::wake::WakeHandle::new();
            let index_wake = wake.clone();
            host.set_index_waker(Arc::new(move || index_wake.wake()));
            self.live = Some(Live::register(&host, Arc::new(move || wake.wake())));
        }
        *self.host.borrow_mut() = Some(host.clone());
        Some(host)
    }

    fn current_host(&self) -> Option<Host> {
        self.host.borrow().clone()
    }

    /// The name a bound control is announced by: the field or view state it edits.
    fn field_names(host: &Rc<RefCell<Option<Host>>>) -> crate::reconcile::FieldNames {
        let shared = Rc::clone(host);
        Rc::new(move |binding: &tilolang_tilo::runtime::Binding| {
            let (target, field) = match &binding.target {
                BindTarget::State { name, .. } => return describe::sentence(name),
                BindTarget::Field { target, field } => (target, field),
            };
            shared
                .borrow()
                .as_ref()
                .and_then(|host| {
                    let (_, ty) = describe::type_of(host, &target.ty)?;
                    ty.fields
                        .iter()
                        .find(|declared| declared.id == *field)
                        .map(|declared| describe::sentence(&declared.name))
                })
                .unwrap_or_default()
        })
    }

    fn case_lookup(host: &Rc<RefCell<Option<Host>>>) -> crate::reconcile::Cases {
        let shared = Rc::clone(host);
        Rc::new(move |id: &str| {
            shared
                .borrow()
                .as_ref()
                .map(|host| describe::cases(host, id))
                .unwrap_or_default()
        })
    }

    /// The window of `target` through `view`.
    fn lane(target: ResourceRef, mut view: ViewInstance) -> Lane {
        show_content_rows(&mut view);
        Lane {
            target,
            with: None,
            view: Rc::new(RefCell::new(view)),
        }
    }

    // ----- later builds --------------------------------------------------------------------

    /// Take over the latest later build of the package (`crate::updates`), when this window
    /// runs an older one: in place when it reads the state the old one wrote, and otherwise by
    /// starting over on the room record `doc` with it, as the app's next start would. The host
    /// the window runs on afterwards.
    fn follow_updates(&mut self, doc: &ui::automerge::AutoCommit, host: Host) -> Host {
        let Some(update) = crate::updates::latest()
            .filter(|update| update.generation > self.generation && self.mode != RunMode::Past)
        else {
            return host;
        };
        self.generation = update.generation;
        let Some(old) = host.package(&self.app) else {
            return host;
        };
        let title = old.meta.title.clone();
        let host = match tilolang_tilo::upgrade::compare(&old.meta, &update.bundle.meta) {
            Upgrade::InPlace => match host.load_bundle(&update.bundle) {
                Ok(_) => {
                    self.reopen(&host);
                    eprintln!("tilolang: {title} runs the new build, with its state");
                    host
                }
                Err(error) => {
                    eprintln!(
                        "tilolang: {title} keeps the last build: the new one does not load: {error}"
                    );
                    return host;
                }
            },
            Upgrade::Restart(reason) => {
                eprintln!(
                    "tilolang: {title}: {reason}; it starts over on the new build, with its data"
                );
                self.start_over(doc).unwrap_or(host)
            }
        };
        ui::app_surface::register_package(&self.app, &update.bundle.surface());
        self.undrawn = Some(Undrawn {
            generation: update.generation,
            read_at: update.read_at,
        });
        self.structure_dirty = true;
        host
    }

    /// Open the root and the detail again on the package `host` now runs: the same resources,
    /// so the same windows and their view state, under the shell that shows them.
    fn reopen(&mut self, host: &Host) {
        let detail = self
            .detail
            .as_ref()
            .map(|lane| (lane.target.clone(), lane.with.clone()));
        let (Ok(target), Ok(view)) = (host.root_of(&self.app), host.open_main(&self.app)) else {
            tracing::warn!(app = self.app, "the new build has no root to open");
            return;
        };
        self.root = Some(Self::lane(target, view));
        self.detail = detail.and_then(|(target, with)| {
            let view = host.open_resource(&target, with.as_deref()).ok()?;
            let mut lane = Self::lane(target, view);
            lane.with = with;
            Some(lane)
        });
        self.lenses_changed = true;
        // What is published and offered follows the new build's declarations.
        if self.live.take().is_some() {
            let wake = ui::wake::WakeHandle::new();
            self.live = Some(Live::register(host, Arc::new(move || wake.wake())));
        }
        self.compose();
        self.commit_planned();
    }

    /// Start the window over on the room record `doc`: a new runtime that loads the latest build
    /// and reads the record afresh, as a start of the app does. View state and the open detail
    /// start over; the record, and so every resource, is the one the window had.
    fn start_over(&mut self, doc: &ui::automerge::AutoCommit) -> Option<Host> {
        self.live = None;
        self.root = None;
        self.detail = None;
        *self.host.borrow_mut() = None;
        if self.sink.borrow().is_none() {
            *self.sink.borrow_mut() = Some(Box::new(LogSink::dev()));
        }
        self.started = false;
        self.host_for(Some(doc))
    }

    /// Open the package's root window.
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
                self.root = Some(Self::lane(target, view));
                self.detail = None;
                self.commit_planned();
            }
            Err(error) => {
                tracing::warn!(app = self.app, %error, "the app did not open");
                self.notice(&t("tilolang-open-failed"));
            }
        }
    }

    /// Show `target` in the window after the root's: what `open(r)` asked for, or a row. With
    /// `front`, a narrow window brings it to the front, moving as what asked for it moves
    /// ([`NavShell::show_from`]).
    fn open_detail(
        &mut self,
        target: &ResourceRef,
        with: Option<&str>,
        front: Option<MotionSource>,
    ) {
        let Some(host) = self.current_host() else {
            return;
        };
        if self
            .detail
            .as_ref()
            .is_some_and(|detail| detail.target == *target && with.is_none())
        {
            if let Some(source) = front {
                self.show_content(source);
            }
            return;
        }
        match host.open_resource(target, with) {
            Ok(view) => {
                let mut lane = Self::lane(target.clone(), view);
                lane.with = with.map(str::to_owned);
                self.detail = Some(lane);
                self.lenses_changed = true;
                self.compose();
                self.commit_planned();
                if let Some(source) = front {
                    self.show_content(source);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "a resource did not open");
                self.notice(&t("tilolang-open-failed"));
            }
        }
    }

    fn show_content(&mut self, source: MotionSource) {
        if let Some(shell) = self.shell() {
            shell.show_from(NavPane::Content, source);
        }
        self.structure_dirty = true;
    }

    fn close_detail(&mut self) {
        if self.detail.take().is_some() {
            self.lenses_changed = true;
            self.compose();
        }
    }

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
        // A script opened it, so a narrow window shows it in front at once rather than sliding
        // it in, however soon after staging the frame is taken.
        if let Some(shown) = shown {
            self.open_detail(&shown, None, Some(MotionSource::Action));
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

    // ----- the shell -----------------------------------------------------------------------

    /// The pane the shell stands in.
    fn shell_pane(&mut self) -> Option<&mut Pane> {
        self.body
            .children_mut()
            .into_iter()
            .next()?
            .as_any_mut()?
            .downcast_mut::<Pane>()
    }

    fn shell(&mut self) -> Option<&mut NavShell> {
        self.shell_pane()?
            .content_mut()
            .as_any_mut()?
            .downcast_mut::<NavShell>()
    }

    fn set_body_slot(&mut self, index: usize, element: Element, fit: Fit) {
        if let Some(arranged) = self.body.as_any_mut().and_then(|any| {
            any.downcast_mut::<ui::widget::arrange::Arranged<ui::widget::arrange::FlexLayout>>()
        }) {
            let mut slots = arranged.take_slots();
            if let Some(target) = slots.get_mut(index) {
                *target = slot(element, fit);
            }
            arranged.set_slots(slots);
        }
        self.structure_dirty = true;
    }

    /// Tell the person something went wrong, as the shell's notice.
    fn notice(&mut self, text: &str) {
        if let Some(shell) = self.shell() {
            shell.toast(text.to_owned(), ToastKind::Error);
        }
    }

    /// Keep the rows of every virtual list of the views, each read from its view, and drop the
    /// rows of lists gone.
    fn track_rows(&self) {
        let mut wanted: Vec<(String, Rc<RefCell<ViewInstance>>, String, &'static str)> = Vec::new();
        for (lane, prefix) in [(self.root.as_ref(), ""), (self.detail.as_ref(), DETAIL)] {
            let Some(lane) = lane else { continue };
            let lists = virtual_lists(lane.view.borrow().tree());
            for (list, _) in lists {
                wanted.push((
                    format!("{prefix}{list}"),
                    Rc::clone(&lane.view),
                    list,
                    prefix,
                ));
            }
        }
        let mut registry = self.rows.borrow_mut();
        registry.retain(|key, rows| {
            wanted
                .iter()
                .any(|(want, view, list, _)| want == key && rows.borrow().reads(view, list))
        });
        for (key, view, list, prefix) in wanted {
            registry.entry(key).or_insert_with(|| {
                let reader = Rc::new(RefCell::new(Reconciler::sharing(&self.reconciler.borrow())));
                Rc::new(RefCell::new(HostRows::of(&view, &list, prefix, reader)))
            });
        }
    }

    /// Bring the shell up to date with the windows of the lane: the root's shell with the
    /// window after it as its content, reconciled into the widgets it was built into.
    fn compose(&mut self) {
        let Some(host) = self.current_host() else {
            return;
        };
        self.track_rows();
        let Some(root) = self.root.as_ref() else {
            return;
        };
        let root_tree = crate::reconcile::normalize(root.view.borrow().tree());
        let root_shell = shell::as_shell(&shell::without_rows(&root_tree));
        let detail_shell = self.detail.as_ref().map(|detail| {
            let tree = crate::reconcile::normalize(detail.view.borrow().tree());
            shell::prefixed(&shell::as_shell(&shell::without_rows(&tree)), DETAIL)
        });
        let composed = shell::compose(&root_shell, detail_shell.as_ref(), BACK_EVENT);
        let composed = {
            let root_view = root.view.borrow();
            let detail_view = self.detail.as_ref().map(|detail| detail.view.borrow());
            let views = shell::Views {
                root: Some(&root_view),
                detail: detail_view.as_deref(),
                host: &host,
            };
            shell::header_rules(composed, &views)
        };
        let old = self.built.take();
        let reconciler = Rc::clone(&self.reconciler);
        if let Some(pane) = self.shell_pane() {
            let mut reconciler = reconciler.borrow_mut();
            match &old {
                Some(old) if old.key == composed.key && old.kind == composed.kind => {
                    if let Some(replaced) = reconciler.update_in(pane.content_mut(), old, &composed)
                    {
                        pane.set(replaced);
                    }
                }
                _ => pane.set(reconciler.build(&composed)),
            }
        }
        self.built = Some(composed);
        self.select_shown();
        self.refresh_bound();
    }

    /// Select, in each list of the overview, the row of the window shown after it: the app
    /// keeps no selection beside it.
    fn select_shown(&mut self) {
        let shown = self.detail.as_ref().map(|detail| detail.target.id_string());
        if let Some(shell) = self.shell()
            && let Some(list) = shell.overview_mut::<DescribedList>()
        {
            list.select_presenting(shown.as_deref());
        }
    }

    /// The Ask bar: trusted chrome naming who asks for what, with the person's choices.
    fn ask_bar(&self, suggestion: &Suggestion) -> Element {
        let action = self
            .current_host()
            .and_then(|host| {
                host.packages().iter().find_map(|package| {
                    package
                        .meta
                        .actions
                        .iter()
                        .find(|action| action.id == suggestion.action)
                        .map(tilolang_tilo::runtime::platform::label_of)
                })
            })
            .unwrap_or_else(|| suggestion.action.clone());
        let by = t_args(
            "tilolang-ask-by",
            &[("invoker", suggestion.invoker.0.as_str().into())],
        );
        let text = |id: &str, value: &str, role: TextRole| {
            slot(
                Box::new(Text::new(value).with_role(role).with_id(id)),
                Fit::hug(),
            )
        };
        let answer = |id: &str, role: ButtonRole, to: Answer| {
            let answers = Rc::clone(&self.answers);
            slot(
                Box::new(
                    Button::new(id, &t(id))
                        .role(role)
                        .on_view(move || answers.borrow_mut().push(to)),
                ),
                Fit::hug(),
            )
        };
        let choices = Row::row()
            .fill_width()
            .push_slot(answer(
                "tilolang-ask-confirm",
                ButtonRole::Primary,
                Answer::Allow,
            ))
            .push_slot(answer(
                "tilolang-ask-decline",
                ButtonRole::Secondary,
                Answer::Decline,
            ))
            .push_slot(answer(
                "tilolang-ask-always",
                ButtonRole::Plain,
                Answer::Always,
            ));
        Box::new(
            Stack::stack()
                .gap(Rhythm::Related)
                .with_id("tilolang-ask")
                .push_slot(text(
                    "tilolang-ask-title",
                    &t("tilolang-ask-title"),
                    TextRole::Heading,
                ))
                .push_slot(text("tilolang-ask-action", &action, TextRole::Body))
                .push_slot(text("tilolang-ask-by", &by, TextRole::Caption))
                .push_slot(slot(Box::new(choices), Fit::hug())),
        )
    }

    fn rebuild_ask(&mut self) {
        let bar: Element = match &self.ask {
            Some(suggestion) => self.ask_bar(suggestion),
            None => Box::new(Absent::new()),
        };
        self.set_body_slot(1, bar, Fit::hug());
    }

    // ----- the views -----------------------------------------------------------------------

    /// Re-run the views whose reads changed, and bring the rows of their virtual lists and the
    /// shell up to date with what changed. Whether anything did.
    fn poll_views(&mut self) -> bool {
        let mut content = false;
        let mut rows_changed = false;
        for (lane, prefix) in [(self.root.as_ref(), ""), (self.detail.as_ref(), DETAIL)] {
            let Some(lane) = lane else { continue };
            let mut view = lane.view.borrow_mut();
            let patches = match view.poll_patches() {
                Ok(patches) => patches,
                Err(error) => {
                    tracing::warn!(%error, "a view failed to run");
                    continue;
                }
            };
            if patches.is_empty() {
                continue;
            }
            let lists = virtual_lists(view.tree());
            let sorted = sort(patches, &lists);
            if sorted.content {
                // A list inside the content may have rows the view has not rendered yet.
                show_content_rows(&mut view);
                content = true;
            }
            drop(view);
            let registry = self.rows.borrow();
            for (list, changes) in sorted.lists {
                rows_changed = true;
                let Some(rows) = registry.get(&format!("{prefix}{list}")) else {
                    content = true;
                    continue;
                };
                let mut rows = rows.borrow_mut();
                if changes.reread {
                    rows.reread();
                } else {
                    rows.apply(&changes.structure);
                    rows.changed(changes.rows);
                }
            }
        }
        if content {
            self.compose();
        }
        content || rows_changed
    }

    /// The windows the views asked to open.
    fn follow_opens(&mut self) {
        let mut opens = Vec::new();
        for lane in [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
        {
            opens.extend(lane.view.borrow_mut().poll_events());
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
                    let front = front.then(by_input);
                    self.open_detail(&target, view.as_deref(), front);
                }
            }
        }
    }

    /// Show what the bound fields and view state hold now in the controls that edit them: the
    /// shell's and the rows its lists show.
    fn refresh_bound(&self) {
        if let Some(built) = &self.built {
            let reconciler = self.reconciler.borrow();
            let root = self.root.as_ref().map(|lane| lane.view.borrow());
            let detail = self.detail.as_ref().map(|lane| lane.view.borrow());
            visit_bound(built, root.as_deref(), detail.as_deref(), &reconciler);
        }
        for rows in self.rows.borrow().values() {
            rows.borrow().refresh_bound();
        }
    }

    /// Bring the lenses the views read up to date with presence: who else is at each resource,
    /// and the person themselves at the window shown after the root's.
    fn publish_lenses(&mut self, peers: HashMap<String, String>) {
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
        if let Some(host) = self.current_host() {
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

    fn take_door_work(&mut self) -> bool {
        if !self.door_work.take() {
            return false;
        }
        self.commit_planned();
        if let Some(host) = self.current_host()
            && let Some(suggestion) = host.take_suspended()
        {
            self.ask = Some(suggestion);
            self.rebuild_ask();
        }
        true
    }

    /// Show what search hits or agents asked the window to show.
    fn take_open_requests(&mut self) -> bool {
        let anchors = std::mem::take(&mut *self.open_requests.borrow_mut());
        if anchors.is_empty() {
            return false;
        }
        let Some(host) = self.current_host() else {
            return false;
        };
        for anchor in anchors {
            // A hit names the published resource (`tl:<app>:<id>`) or the resource's id.
            let anchor = anchor
                .strip_prefix("tl:")
                .and_then(|rest| rest.split_once(':'))
                .map_or(anchor.as_str(), |(_, id)| id)
                .to_owned();
            match host.open_plan(&anchor) {
                Ok(plan) => {
                    if let Some(target) = plan.target
                        && (!target.path.is_empty() || plan.app != self.app)
                    {
                        // A search hit or an agent's `open`: no hand moved it, it lands at once.
                        self.open_detail(&target, None, Some(MotionSource::Action));
                    }
                    self.commit_planned();
                }
                Err(error) => {
                    tracing::warn!(%error, "a search hit named nothing in the room");
                    self.notice(&t("tilolang-open-failed"));
                }
            }
        }
        true
    }

    fn settle_outbox(&mut self) -> bool {
        // What waits in the record is settled once the window reads the record.
        if self.opening.is_some()
            || !std::mem::take(&mut self.settle_due)
            || self.mode != RunMode::Live
        {
            return false;
        }
        let Some(host) = self.current_host() else {
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

    /// Hand what the runtime committed to the door. Returns the tickets handed over.
    fn commit_planned(&mut self) -> Vec<u64> {
        // A past version shows what it held; nothing it does is committed.
        if self.mode == RunMode::Past {
            return Vec::new();
        }
        let Some(host) = self.current_host() else {
            return Vec::new();
        };
        let tickets = queue_commits(&host, &self.outgoing, 0, None);
        self.settle_due = true;
        tickets
    }

    /// The door answered the oldest commit handed over.
    fn door_answered(&mut self, error: Option<String>) {
        let Some(ticket) = self.in_flight.pop_front() else {
            return;
        };
        if let Some(host) = self.current_host() {
            let _ = host.store().take_ticket(ticket);
        }
        let undo = self.pending.undo.remove(&ticket);
        match (error, undo) {
            (Some(error), _) => self.notice(&error),
            (None, Some(notice)) => {
                // A destructive action is undone rather than confirmed.
                if let Some(shell) = self.shell() {
                    shell.toast_with_action(notice, ToastKind::Info, "undo-2", t("tilolang-undo"));
                }
            }
            (None, None) => {}
        }
    }

    // ----- what the person did -------------------------------------------------------------

    fn handle_events(&mut self) -> bool {
        let mut changed = false;
        let answers = std::mem::take(&mut *self.answers.borrow_mut());
        for answer in answers {
            changed = true;
            self.answer(answer);
        }
        let mut events: Vec<UiEvent> = std::mem::take(&mut *self.events.borrow_mut());
        events.extend(self.reconciler.borrow().adapter().take_events());
        for event in events {
            changed = true;
            self.event(&event);
        }
        changed
    }

    /// The lane a key of the composed shell is of, and the key in that lane's view.
    fn lane_of<'k>(&self, key: &'k str) -> Option<(Rc<RefCell<ViewInstance>>, &'k str)> {
        match key.strip_prefix(DETAIL) {
            Some(own) => Some((Rc::clone(&self.detail.as_ref()?.view), own)),
            None => Some((Rc::clone(&self.root.as_ref()?.view), key)),
        }
    }

    fn event(&mut self, event: &UiEvent) {
        if let UiEvent::Fire(key, name, _) = event
            && *name == shell::BACK
            && self.built.as_ref().is_some_and(|built| built.key == *key)
        {
            self.close_detail();
            return;
        }
        let key = match event {
            UiEvent::Press(key)
            | UiEvent::Edit(key, _)
            | UiEvent::Toggle(key, _)
            | UiEvent::Resolve(key, _)
            | UiEvent::Fire(key, ..) => Some(key.as_str()),
            UiEvent::Open(_) => None,
        };
        let lane = key.and_then(|key| self.lane_of(key));
        let first = self.current_host().map(|host| host.next_ticket());
        let mut ran: Option<(ActionRef, Value)> = None;
        // A press and a component's event apply the action reference the view attached: done,
        // denied, or suggested for a person to decide.
        let outcome = match (event, &lane) {
            (UiEvent::Press(_), Some((view, key))) => self.fire(view, key, "press", None, &mut ran),
            (UiEvent::Fire(_, name, value), Some((view, key))) => {
                self.fire(view, key, name, value.as_ref(), &mut ran)
            }
            (UiEvent::Edit(_, text), Some((view, key))) => {
                edit(&view.borrow(), key, Value::Text(text.clone()))
            }
            (UiEvent::Toggle(_, checked), Some((view, key))) => {
                edit(&view.borrow(), key, Value::Bool(*checked))
            }
            (UiEvent::Resolve(_, approve), Some((view, key))) => {
                self.resolve(&view.borrow(), key, *approve)
            }
            (UiEvent::Open(target), _) => {
                let target = target.clone();
                self.open_detail(&target, None, Some(by_input()));
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(message) = outcome {
            self.notice(&message);
            self.refresh_bound();
        }
        let tickets = self.commit_planned();
        if let (Some((aref, result)), Some(first)) = (ran, first) {
            self.after_action(&aref, &result, tickets.into_iter().filter(|t| *t >= first));
        }
        self.follow_opens();
    }

    /// Fire the event `name` of the node `key` of `view`, carrying `value`; `ran` takes the
    /// action it applied and what that returned, when it ran one.
    fn fire(
        &mut self,
        view: &Rc<RefCell<ViewInstance>>,
        key: &str,
        name: &str,
        value: Option<&Value>,
        ran: &mut Option<(ActionRef, Value)>,
    ) -> Result<(), String> {
        let action = view.borrow().event_action(key, name);
        let fired = view.borrow().fire_with(key, name, value);
        match fired {
            Ok(InvokeOutcome::Done(result)) => {
                *ran = action.map(|action| (action, result));
                Ok(())
            }
            Ok(InvokeOutcome::Suggested(suggestion)) => {
                self.ask = Some(suggestion);
                self.rebuild_ask();
                Ok(())
            }
            Ok(InvokeOutcome::Denied) => Err(t("tilolang-ask-denied")),
            Err(error) => Err(error.to_string()),
        }
    }

    /// What follows an action a person ran from a control: a destructive one offers Undo once
    /// its commits (`tickets`) are in; what a primary one made is shown.
    fn after_action(
        &mut self,
        aref: &ActionRef,
        result: &Value,
        tickets: impl Iterator<Item = u64>,
    ) {
        let Some(host) = self.current_host() else {
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
                let label = tilolang_tilo::runtime::platform::label_of(&action);
                // What it was run on: the item of a list (`delete(nb, n)`), or the resource.
                let subject = aref.args.iter().rev().find_map(|arg| match arg {
                    Value::Ref(subject) => Some(ResourceRef::clone(&subject.target)),
                    _ => None,
                });
                let title = subject
                    .map(|subject| describe::title_of(&host, &subject))
                    .unwrap_or_default();
                let notice = if title.is_empty() {
                    label
                } else {
                    t_args(
                        "tilolang-did",
                        &[
                            ("action", label.as_str().into()),
                            ("title", title.as_str().into()),
                        ],
                    )
                };
                if let Some(last) = tickets.max() {
                    self.pending.undo.insert(last, notice);
                }
            }
            Some("primary") => {
                // What a primary action made opens beside the list, as the person asked for it.
                if let Value::Ref(made) = result {
                    self.open_requests
                        .borrow_mut()
                        .push(made.target.id_string());
                }
            }
            _ => {}
        }
    }

    fn resolve(&self, view: &ViewInstance, key: &str, approve: bool) -> Result<(), String> {
        let Some(host) = self.current_host() else {
            return Ok(());
        };
        let target = view
            .tree()
            .find(key)
            .and_then(|card| match card.props.get("invocation") {
                Some(Value::Ref(reference)) => Some(reference.target.clone()),
                _ => None,
            })
            .ok_or_else(|| t("tilolang-open-failed"))?;
        let resolution = if approve {
            Resolution::Approve
        } else {
            Resolution::Decline
        };
        host.runtime()
            .resolve_invocation(&target, &Principal::new(self.me.clone()), resolution)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn answer(&mut self, answer: Answer) {
        let (Some(suggestion), Some(host)) = (self.ask.take(), self.current_host()) else {
            return;
        };
        let resolution = match answer {
            Answer::Decline => Resolution::Decline,
            Answer::Allow | Answer::Always => {
                if answer == Answer::Always {
                    for author in &suggestion.chain {
                        host.authority().trust(author);
                    }
                    host.always_allow_agents(&suggestion.action);
                }
                Resolution::Approve
            }
        };
        if let Err(error) = host.runtime().resolve(
            &suggestion.run,
            &Principal::new(self.me.clone()),
            resolution,
        ) {
            self.notice(&error.to_string());
        }
        self.rebuild_ask();
        self.commit_planned();
    }

    /// The shell's Undo after a destructive action, when the person chose it.
    fn take_toast_undo(&mut self) {
        let taken = self.shell().and_then(NavShell::take_toast_action);
        if taken.is_some() {
            self.undo.request(ui::undo::UndoRequest::Undo);
        }
    }

    /// The door answered an invocation the window handed over: done, or refused with `error`.
    /// Whether it was the window's own (a commit).
    fn answered(&mut self, address: &ActionAddress, error: Option<String>) -> bool {
        if address.resource == HOST_RESOURCE && address.action == COMMIT {
            self.door_answered(error);
            return true;
        }
        if let Some(error) = error
            && address.resource.starts_with("tl:")
        {
            self.notice(&error);
            return true;
        }
        false
    }

    // ----- for tests -----------------------------------------------------------------------

    /// The root view's tree, as the window draws it: the rows of its lists are those the lists
    /// show.
    #[must_use]
    pub fn root_tree(&self) -> Option<UiNode> {
        self.root
            .as_ref()
            .map(|lane| crate::reconcile::normalize(lane.view.borrow().tree()))
    }

    /// The tree of the window after the root's.
    #[must_use]
    pub fn detail_tree(&self) -> Option<UiNode> {
        self.detail
            .as_ref()
            .map(|lane| crate::reconcile::normalize(lane.view.borrow().tree()))
    }

    /// The composed shell, as the window last built it.
    #[must_use]
    pub fn composed(&self) -> Option<&UiNode> {
        self.built.as_ref()
    }

    /// The resource the window after the root's shows.
    #[must_use]
    pub fn detail_target(&self) -> Option<&ResourceRef> {
        self.detail.as_ref().map(|lane| &lane.target)
    }

    /// Report what a person did: an event of a control of the composed shell, by its key there.
    pub fn act(&self, event: UiEvent) {
        self.events.borrow_mut().push(event);
    }

    /// The shell.
    #[must_use]
    pub fn shell_ref(&self) -> Option<&NavShell> {
        self.body
            .children()
            .into_iter()
            .next()?
            .as_any()?
            .downcast_ref::<Pane>()?
            .content()
            .as_any()?
            .downcast_ref::<NavShell>()
    }

    /// The shell, to change it as a person would.
    pub fn shell_mut(&mut self) -> Option<&mut NavShell> {
        self.shell()
    }

    /// The host, once the window has the record.
    #[must_use]
    pub fn host(&self) -> Option<Host> {
        self.current_host()
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
    lists: std::collections::BTreeMap<String, ListChanges>,
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

fn edit(view: &ViewInstance, key: &str, value: Value) -> Result<(), String> {
    let value = match (&value, view.bound_value(key)) {
        (Value::Text(text), Some(Value::Int(_))) => {
            Value::Int(text.trim().parse().map_err(|_| String::new())?)
        }
        (Value::Text(text), Some(Value::Float(_))) => {
            Value::Float(text.trim().parse().map_err(|_| String::new())?)
        }
        _ => value,
    };
    match view.edit(key, &value) {
        Ok(EditOutcome::Applied) => Ok(()),
        Ok(EditOutcome::NeedsApproval) => Err(t("tilolang-ask-needs-approval")),
        Ok(EditOutcome::Denied) => Err(t("tilolang-ask-denied")),
        Err(error) => Err(error.to_string()),
    }
}

/// Show what each control of the composed shell `node` is bound to now, read from its view.
fn visit_bound(
    node: &UiNode,
    root: Option<&ViewInstance>,
    detail: Option<&ViewInstance>,
    reconciler: &Reconciler,
) {
    if node.binding.is_some() {
        let (view, key) = match node.key.strip_prefix(DETAIL) {
            Some(own) => (detail, own),
            None => (root, node.key.as_str()),
        };
        if let Some(value) = view.and_then(|view| view.bound_value(key)) {
            match value {
                Value::Bool(flag) => reconciler.set_flag(&node.key, flag),
                Value::Int(0) if node.kind == "checkbox" || node.kind == "toggle" => {
                    reconciler.set_flag(&node.key, false);
                }
                Value::Int(1) if node.kind == "checkbox" || node.kind == "toggle" => {
                    reconciler.set_flag(&node.key, true);
                }
                other => reconciler.set_text(&node.key, &crate::reconcile::display(&other)),
            }
        }
    }
    for child in &node.children {
        visit_bound(child, root, detail, reconciler);
    }
}

/// Where in the package a participant is: the resource their window shows beside the root.
fn presence_key(app: &str, target: &ResourceRef) -> ui::presence::PresenceKey {
    ui::presence::PresenceKey::Resource {
        app: app.to_owned(),
        id: target.id_string(),
        kind: target.ty.clone(),
    }
}

/// The participants other than `me` at each resource of the package `app`.
fn peers_of(presence: &ui::presence::PresenceCtx, app: &str, me: &str) -> HashMap<String, String> {
    presence
        .selections
        .iter()
        .filter_map(|(key, at)| match key {
            ui::presence::PresenceKey::Resource { app: of, id, .. } if of == app => at
                .iter()
                .find(|(user, _)| user.0 != me)
                .map(|(user, _)| (id.clone(), user.0.clone())),
            _ => None,
        })
        .collect()
}

impl Drop for Window {
    /// What the indexes hold reaches the disk as the window closes: the next window of the room
    /// opens over them before its record.
    fn drop(&mut self) {
        if let Some(host) = self.current_host() {
            host.persist_index();
        }
    }
}

impl ui::presence::capability::PublishesPresence for Window {
    /// The resource the window shows beside its root, so a collaborator's list shows this
    /// participant at its row.
    fn presence_payload(
        &self,
    ) -> Option<(
        ui::presence::PresenceKey,
        ui::collaboration::PresencePayload,
    )> {
        let detail = self.detail.as_ref()?;
        Some((
            presence_key(&self.app, &detail.target),
            ui::collaboration::PresencePayload::Selection { start: 0, end: 0 },
        ))
    }
}

impl ui::widget::controls::ActionControls for Window {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.take_attributed_control_invocation()
            .map(|(invocation, _)| invocation)
    }

    fn take_attributed_control_invocation(
        &mut self,
    ) -> Option<ui::widget::controls::AttributedControlInvocation> {
        self.take_door_work();
        let Some((invocation, origin)) = self.outgoing.borrow_mut().pop_front() else {
            return ui::widget::controls::take_attributed_control_invocation(self.body.as_mut());
        };
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

    fn on_control_invocation_result(&mut self, address: &ActionAddress, result: &ControlResult) {
        let error = result.as_ref().err().map(ToString::to_string);
        if !self.answered(address, error) {
            ui::widget::controls::on_control_invocation_result(self.body.as_mut(), address, result);
        }
    }
}

impl ui::undo::UndoControl for Window {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> ui::undo::UndoStatus) {
        self.undo.publish(&status());
    }

    fn take_undo_request(&mut self) -> Option<ui::undo::UndoRequest> {
        self.undo.take_request()
    }
}

impl ResourceSurface<Room> for Window {
    fn present(&mut self, _state: &Rc<RoomState>) {
        // The record moved: what the views read may have changed, and outside effects may wait.
        self.moved = true;
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.staged = scenario.map(str::to_owned);
    }
}

impl Widget for Window {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::undo::UndoControl,
        dyn ui::presence::capability::PublishesPresence
    );

    fn name(&self) -> &'static str {
        "TilolangWindow"
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.body.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![self.body.as_mut()]
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let _phase = Phase::start("tl.sync");
        // Shown before the room's record, the window reads none: it opens over the indexes.
        let doc = self
            .opening
            .is_none()
            .then(|| ui::doc::document_ref(state, path).0);
        let Some(host) = self.host_for(doc) else {
            self.body.sync(state, path);
            return;
        };
        let host = match doc {
            Some(doc) => self.follow_updates(doc, host),
            None => host,
        };
        if !self.started {
            // Until the indexes are read from, the lists would show nothing: the window shows
            // the platform's loading state (or, before the room's first sync, that it connects).
            if host.index_ready() {
                self.started = true;
                self.loading = false;
                self.open_root(&host);
                self.compose();
            } else if !self.loading {
                self.loading = true;
                let loading = ui::widget::EmptyState::new(ui::i18n::I18nManager::lookup_plain(
                    "arriving-loading",
                ))
                .busy();
                if let Some(pane) = self.shell_pane() {
                    pane.set(Box::new(loading));
                }
                self.structure_dirty = true;
            }
        }
        if self.staged.is_some() {
            self.run_staged(&host);
        }
        if std::mem::take(&mut self.moved) {
            self.refresh_bound();
            self.settle_due = true;
            if let Some(live) = &self.live {
                live.update();
            }
        } else if let Some(live) = self.live.as_ref().filter(|live| live.pending()) {
            live.update();
        }
        let views = Phase::start("tl.views");
        self.poll_views();
        drop(views);
        self.body.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let _phase = Phase::start("tl.update");
        let door = self.take_door_work();
        let opened = self.take_open_requests();
        let settled = self.settle_outbox();
        let renamed = self.names_changed.take();
        if renamed {
            for lane in [self.root.as_ref(), self.detail.as_ref()]
                .into_iter()
                .flatten()
            {
                lane.view.borrow().refresh();
            }
        }
        // Who is at a resource shows wherever a view reads its lenses.
        let lenses = std::mem::take(&mut self.lenses_changed);
        let handled = Phase::start("tl.events");
        let changed = self.handle_events() || door || opened || settled || lenses || renamed;
        drop(handled);
        if changed {
            let _views = Phase::start("tl.views");
            self.poll_views();
        }
        self.take_toast_undo();
        let child = self.body.update(state, path);
        changed || child
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.body.handle_event(event, ctx);
    }

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.body.build_layout(tree, theme, nodes)
    }

    fn apply_layout(
        &mut self,
        tree: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.body.apply_layout(tree, node, origin);
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.body.get_rect()
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.names.follow(presence) {
            self.names_changed.set(true);
        }
        let peers = peers_of(presence, &self.app, &self.me);
        self.publish_lenses(peers);
        self.body.draw(scene, transform, text, theme, presence);
        if let Some(undrawn) = self.undrawn.take() {
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            eprintln!(
                "tilolang: {} draws build {} at {} ms, {} ms after it arrived",
                self.app,
                undrawn.generation,
                at.as_millis(),
                undrawn.read_at.elapsed().as_millis()
            );
        }
    }

    /// The window paints nothing of its own: what changed is what its body
    /// repaints, not the whole window whenever anything in it changed.
    fn collect_damage(&self, regions: &mut Vec<ui::kurbo::Rect>) {
        self.body.collect_damage(regions);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.structure_dirty);
        self.body.take_structure_dirty() || dirty
    }

    /// The window's own changes (a shell built, a bar shown) rebuild; what its body describes
    /// (a column's content changed in place) stays described.
    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        let own = ui::widget::StructureChange::of(std::mem::take(&mut self.structure_dirty));
        own.join(self.body.take_structure_change())
    }
}
