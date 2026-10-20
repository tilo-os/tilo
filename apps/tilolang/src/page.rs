//! The host's window: a home view listing the packages, and the window of any package's root
//! resource, shown through the default view of its type.
//!
//! `open(r)` in a view goes to the window of `r`. Until the workspace's window model lands this
//! window shows it in place, through the default view of `r`'s type, and its back button
//! returns to the view it was opened from.
//!
//! The widget owns a [`Host`] (the runtime on the platform), the open [`ViewInstance`] and the
//! retained widgets of its tree. Each frame it
//!
//! 1. brings the runtime's mirror of the room record up to date with the record,
//! 2. re-runs the view if what it read changed and reconciles the new tree by key,
//! 3. takes what the person did (a press, an edit, a toggle, an answer to the Ask bar) and runs it
//!    in the runtime, which plans the change;
//! 4. commits what the runtime planned through the action door, as an invocation of the host's
//!    `commit` action, so the change is attributed, undoable and replayed by every peer.
//!
//! The Ask bar is drawn here, in Rust, from toolkit controls and fluent strings. A view cannot
//! draw it, so a package cannot imitate it.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value as Json;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{
    COMMIT, Caller, Committer, HOST_RESOURCE, Planner, host_resource, publish_package,
    publish_record, publish_tree,
};
use tilolang_tilo::runtime::{
    ActionInfo, BindTarget, Decision, EditOutcome, InvokeOutcome, OutsideSink, Principal,
    Resolution, Suggestion, UiNode, Value, ViewEvent, ViewInstance,
};
use tilolang_tilo::sink::LogSink;
use tilolang_tilo::{HostEffect, OpenEffect};
use ui::LocalState;
use ui::search::SearchCatalog;
use ui::tilo_actions::{
    ActionAddress, ActionInvoker, ActorKind, ControlInvocation, InvocationId, Resource,
};
use ui::tokens::{Rhythm, TextRole};
use ui::widget::arrange::{
    Arranged, Fit, FlexLayout, Grid, Length, Navigation, Page as PageLayout, Pane, Row, Slot,
    Space, Stack, WidthClass, rem_of,
};
use ui::widget::controls::ControlResult;
use ui::widget::{
    AppKind, AppModel, AppWidget, Breadcrumb, Button, ButtonRole, Element, IconDescriptor,
    ModelPath, Spacer, Text, Widget,
};

mod stage;
pub use stage::SCENARIOS;

use crate::i18n::t;
use crate::names::Names;
#[cfg(test)]
use crate::packages::EMBEDDED;
use crate::reconcile::{EventSink, Reconciler, UiEvent};

/// Where the window is.
enum Mode {
    Home,
    Open {
        app: String,
        view: Box<ViewInstance>,
        title: String,
        /// The views `open(r)` left, the most recent last: back returns to them.
        trail: Vec<ViewInstance>,
    },
}

/// A navigation the person asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Nav {
    Open(String),
    Home,
    /// To the view an `open(r)` left, or home from the first one.
    Back,
}

/// An answer to the Ask bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Allow,
    Decline,
    Always,
}

/// The queues the controls of the window report into.
#[derive(Clone, Default)]
struct Inbox {
    nav: Rc<RefCell<Vec<Nav>>>,
    answers: Rc<RefCell<Vec<Answer>>>,
}

/// Commits waiting to be handed to the door. One an invocation of the door made is tagged with
/// it, so the framework applies it for the invoker the door admitted (an agent's task, for the
/// person it acts for), in the context it carried, not as the person at the window.
type Outgoing = Rc<RefCell<VecDeque<(ControlInvocation, Option<InvocationId>)>>>;

/// Hand everything the runtime committed under a ticket of `first` or later to the door, tagged
/// with the invocation that made it.
fn queue_commits(host: &Host, outgoing: &Outgoing, first: u64, origin: Option<InvocationId>) {
    for committed in host.take_committed_from(first) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("ticket".to_owned(), Json::from(committed.ticket));
        if let Some(invocation) = ControlInvocation::from_parts(HOST_RESOURCE, COMMIT, arguments) {
            outgoing.borrow_mut().push_back((invocation, origin));
        }
    }
}

/// What the door admitted of an invoker, as the host reads it.
fn caller_of(invoker: &ActionInvoker) -> Caller {
    Caller {
        actor: invoker.actor.clone(),
        agent: invoker.kind != ActorKind::Person,
        on_behalf_of: invoker.on_behalf_of.clone(),
    }
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
    door_work: &Rc<std::cell::Cell<bool>>,
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
            // What the effect committed is applied for its invoker as soon as the host
            // answers, the way the framework applies what an effect's handler submits.
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

/// The window. See the module docs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent frame flags of one window: started, dirty, detail shown or pending"
)]
pub struct TilolangPage {
    me: String,
    host: Rc<RefCell<Option<Host>>>,
    mode: Mode,
    reconciler: Reconciler,
    events: EventSink,
    inbox: Inbox,
    /// The tree the body shows, and the widgets built for it hang in `root`.
    body_tree: Option<UiNode>,
    /// Header, body, chrome.
    root: Element,
    structure_dirty: bool,
    /// Commits waiting to be handed to the door, and the tickets handed over and not yet answered.
    outgoing: Outgoing,
    in_flight: VecDeque<u64>,
    ask: Option<Suggestion>,
    notice: Option<String>,
    /// The width class the open view was last laid out in, for hysteresis.
    class: Option<WidthClass>,
    rem: f32,
    last_size: Option<(f32, f32)>,
    /// Whether the first frame built the host.
    started: bool,
    /// The room record moved while the host was fetched outside `sync`.
    moved: std::cell::Cell<bool>,
    /// Set when the door asked the host to do something that the runtime committed: the commits
    /// are then handed to the door like a press's.
    door_work: Rc<std::cell::Cell<bool>>,
    /// The resources search hits asked the window to open, until the next frame.
    open_requests: Rc<RefCell<Vec<String>>>,
    /// Outside effects may be waiting in the record for this device to carry out.
    settle_due: bool,
    /// Where the outside effects of this device go once settled.
    sink: RefCell<Option<Box<dyn OutsideSink>>>,
    /// This device, as the room's record names the owner of an outside effect.
    device: Option<String>,
    /// What search finds in the room, kept as the record moves.
    search: SearchCatalog,
    /// What the system's persistent search index keeps of the room.
    index: RefCell<Vec<ui::search::index_publication::IndexEntry>>,
    /// What the room's participants are called, and whether a frame brought a new name.
    names: Names,
    names_changed: std::cell::Cell<bool>,
    /// The split of the open view that shows one pane at a time when narrow, and whether the
    /// header offers the way back from its detail pane.
    navigation: Option<Navigation>,
    detail_shown: bool,
    /// A press in the list pane may open something in the detail pane: go there once it does.
    pending_detail: bool,
    /// The installed package this window runs: the app it is. Home and the other packages exist
    /// only in the window's own tests, which run the built-in packages side by side.
    runs: Option<Arc<tilolang_tilo::bundle::Bundle>>,
    /// What a capture stages before it is drawn: empty on every live path.
    stage: VecDeque<stage::Step>,
}

fn slot(element: Element, fit: Fit) -> Slot {
    let mut slot = Slot::new(element);
    slot.fit = fit;
    slot
}

impl TilolangPage {
    /// A window that stages `scenario` before it is first drawn: for captures only.
    #[must_use]
    pub fn staged(me: String, scenario: &str) -> Self {
        let mut page = Self::new(me);
        page.stage = stage::script(scenario);
        page
    }

    #[must_use]
    pub fn new(me: String) -> Self {
        let events: EventSink = Rc::default();
        let host: Rc<RefCell<Option<Host>>> = Rc::default();
        let shared = Rc::clone(&host);
        // A bound control is announced by the name of the field or view state it edits.
        let names = Rc::new(move |binding: &tilolang_tilo::runtime::Binding| {
            let (target, field) = match &binding.target {
                BindTarget::State { name, .. } => return name.clone(),
                BindTarget::Field { target, field } => (target, field),
            };
            shared
                .borrow()
                .as_ref()
                .and_then(|host| {
                    host.packages().into_iter().find_map(|package| {
                        let declared = package
                            .meta
                            .types
                            .iter()
                            .find(|declared| declared.id == target.ty)?;
                        declared
                            .fields
                            .iter()
                            .find(|declared| declared.id == *field)
                            .map(|declared| declared.name.clone())
                    })
                })
                .unwrap_or_default()
        });
        let open_requests: Rc<RefCell<Vec<String>>> = Rc::default();
        let door_work: Rc<std::cell::Cell<bool>> = Rc::default();
        let outgoing: Outgoing = Rc::default();
        let (endpoint, open_endpoint) =
            door_endpoints(&host, &door_work, &outgoing, &open_requests);
        let root = Stack::stack()
            .fill_width()
            .fill_height()
            .gap(Rhythm::Tight)
            .push_slot(slot(Box::new(Spacer::fixed(0.0)), Fit::hug()))
            .push_slot(slot(Box::new(Spacer::fixed(0.0)), Fit::fill(1.0)))
            .push_slot(slot(Box::new(Spacer::fixed(0.0)), Fit::hug()))
            .push_slot(slot(endpoint, Fit::hug()))
            .push_slot(slot(open_endpoint, Fit::hug()));
        Self {
            names: Names::new(&me),
            me,
            host,
            mode: Mode::Home,
            reconciler: Reconciler::new(Rc::clone(&events), names),
            events,
            inbox: Inbox::default(),
            body_tree: None,
            root: Box::new(root),
            structure_dirty: true,
            outgoing,
            in_flight: VecDeque::new(),
            ask: None,
            notice: None,
            class: None,
            rem: 16.0,
            last_size: None,
            started: false,
            moved: std::cell::Cell::new(false),
            door_work,
            open_requests,
            settle_due: false,
            sink: RefCell::new(Some(Box::new(LogSink::dev()))),
            device: None,
            search: SearchCatalog::new("tilolang"),
            index: RefCell::default(),
            names_changed: std::cell::Cell::new(false),
            navigation: None,
            detail_shown: false,
            pending_detail: false,
            runs: None,
            stage: VecDeque::new(),
        }
    }

    /// This window on the device `device`: the one that carries out the outside effects its
    /// room queues.
    #[must_use]
    pub fn on_device(mut self, device: &str) -> Self {
        self.device = Some(device.to_owned());
        self
    }

    /// This window sending the outside effects it settles to `sink` rather than the development
    /// log.
    #[must_use]
    pub fn with_sink(self, sink: impl OutsideSink + 'static) -> Self {
        *self.sink.borrow_mut() = Some(Box::new(sink));
        self
    }

    /// The window of the installed app `bundle`: it runs that one package and opens its root.
    #[must_use]
    pub fn running(mut self, bundle: Arc<tilolang_tilo::bundle::Bundle>) -> Self {
        self.runs = Some(bundle);
        self
    }

    /// The host over the room record, built (with every embedded package) the first time the
    /// record is at hand.
    fn host_for(&self, doc: &ui::automerge::AutoCommit) -> Host {
        if let Some(host) = self.host.borrow().as_ref() {
            if host.sync_from(doc) {
                self.moved.set(true);
            }
            return host.clone();
        }
        let built = Host::on_device(
            doc,
            Principal::new(self.me.clone()),
            self.device.as_deref(),
            |_: &Principal, _: &ActionInfo| {
                // What the person may do in the room is decided where the change is committed: the
                // door refuses a viewer's `commit`. Code authorship is decided by the runtime.
                Decision::Allow
            },
        );
        let host = match built {
            Ok(host) => host,
            Err(error) => {
                tracing::error!(%error, "the tilolang runtime could not be set up");
                // Without a runtime the window lists nothing; surface the failure.
                let message = error.to_string();
                return Self::failed_host(doc, &message);
            }
        };
        host.runtime().set_name_host(self.names.clone());
        if let Some(sink) = self.sink.borrow_mut().take() {
            host.runtime().set_sink(BoxedSink(sink));
        }
        match &self.runs {
            Some(bundle) => {
                if let Err(error) = host.load_bundle(bundle) {
                    tracing::error!(app = bundle.meta.app, %error, "the installed package did not load");
                }
            }
            #[cfg(test)]
            None => {
                for package in EMBEDDED {
                    if let Err(error) = host.load_builtin(package.source, package.ids) {
                        tracing::error!(package = package.dir, %error, "a built-in package did not load");
                    }
                }
            }
            #[cfg(not(test))]
            None => tracing::error!("no package is installed in this runtime"),
        }
        *self.host.borrow_mut() = Some(host.clone());
        host
    }

    /// A host with no packages, after the runtime could not be set up. Only reached by a bug.
    fn failed_host(doc: &ui::automerge::AutoCommit, _why: &str) -> Host {
        // `Host::new` failing means the engine itself failed; trying again gives the same error,
        // so there is nothing to show but the empty home.
        match Host::new(doc, Principal::new(String::new()), |_, _| Decision::Deny) {
            Ok(host) => host,
            Err(error) => {
                tracing::error!(%error, "the tilolang runtime is unavailable");
                std::process::abort()
            }
        }
    }

    fn current_host(&self) -> Option<Host> {
        self.host.borrow().clone()
    }

    /// The view tree the window shows, when an app is open.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> Option<&UiNode> {
        self.body_tree.as_ref()
    }

    /// The app the window shows.
    #[cfg(test)]
    pub(crate) fn open_app(&self) -> Option<&str> {
        match &self.mode {
            Mode::Open { app, .. } => Some(app),
            Mode::Home => None,
        }
    }

    /// Report what a person did to the view, as its controls do.
    #[cfg(test)]
    pub(crate) fn act(&self, event: UiEvent) {
        self.events.borrow_mut().push(event);
    }

    /// The door answered the oldest invocation handed over: the queued action is the record's
    /// now, or it was refused with `error`.
    pub(crate) fn door_answered(&mut self, error: Option<String>) {
        let Some(ticket) = self.in_flight.pop_front() else {
            return;
        };
        if let Some(host) = self.current_host() {
            let _ = host.store().take_ticket(ticket);
        }
        if let Some(error) = error {
            self.notice = Some(error);
            self.rebuild_chrome();
        }
    }

    // ----- body, header and chrome ---------------------------------------------------------

    fn set_slot(&mut self, index: usize, element: Element, fit: Fit) {
        if let Some(arranged) = self
            .root
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Arranged<FlexLayout>>())
        {
            let mut slots = arranged.take_slots();
            if let Some(target) = slots.get_mut(index) {
                *target = slot(element, fit);
            }
            arranged.set_slots(slots);
        }
        self.structure_dirty = true;
    }

    fn take_body(&mut self) -> Option<Element> {
        let arranged = self
            .root
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Arranged<FlexLayout>>())?;
        let mut slots = arranged.take_slots();
        let body = slots.get_mut(1).map(|target| {
            std::mem::replace(target, slot(Box::new(Spacer::fixed(0.0)), Fit::fill(1.0)))
                .into_element()
        });
        arranged.set_slots(slots);
        body
    }

    fn rebuild_header(&mut self) {
        let header: Element = match self.mode {
            Mode::Home => Box::new(Spacer::fixed(0.0)),
            Mode::Open {
                ref title,
                ref trail,
                ..
            } => {
                let nav = Rc::clone(&self.inbox.nav);
                let detailed = self.detail_shown || !trail.is_empty();
                let mut segments = vec![t("tilolang-back"), title.clone()];
                if detailed {
                    segments.push(t("tilolang-detail"));
                }
                let crumbs = Breadcrumb::new(segments).on_navigate(move |index| match index {
                    0 => nav.borrow_mut().push(Nav::Home),
                    1 if detailed => nav.borrow_mut().push(Nav::Back),
                    _ => {}
                });
                Box::new(
                    PageLayout::full()
                        .with_id("tilolang-header")
                        .push_slot(slot(Box::new(crumbs), Fit::hug())),
                )
            }
        };
        self.set_slot(0, header, Fit::hug());
    }

    fn rebuild_chrome(&mut self) {
        let mut chrome = Stack::stack()
            .gap(Rhythm::Related)
            .with_id("tilolang-chrome");
        if let Some(suggestion) = &self.ask {
            chrome = chrome.push_slot(slot(self.ask_bar(suggestion), Fit::hug()));
        }
        if let Some(notice) = &self.notice {
            chrome = chrome.push_slot(slot(
                Box::new(
                    Text::new(notice)
                        .with_role(TextRole::Caption)
                        .with_id("tilolang-notice"),
                ),
                Fit::hug(),
            ));
        }
        self.set_slot(2, Box::new(chrome), Fit::hug());
    }

    /// The Ask bar: trusted chrome naming who asks for what, with the person's choices.
    fn ask_bar(&self, suggestion: &Suggestion) -> Element {
        let mut bar = Stack::stack().gap(Rhythm::Related).with_id("tilolang-ask");
        bar = bar.push_slot(slot(
            Box::new(
                Text::new(&t("tilolang-ask-title"))
                    .with_role(TextRole::Heading)
                    .with_id("tilolang-ask-title"),
            ),
            Fit::hug(),
        ));
        let action = self
            .current_host()
            .and_then(|host| {
                host.packages().iter().find_map(|package| {
                    package
                        .meta
                        .actions
                        .iter()
                        .find(|action| action.id == suggestion.action)
                        .map(|action| action.name.clone())
                })
            })
            .unwrap_or_else(|| suggestion.action.clone());
        let by = crate::i18n::t_args(
            "tilolang-ask-by",
            &[("invoker", suggestion.invoker.0.as_str().into())],
        );
        bar = bar.push_slot(slot(
            Box::new(
                Text::new(&action)
                    .with_role(TextRole::Body)
                    .with_id("tilolang-ask-action"),
            ),
            Fit::hug(),
        ));
        bar = bar.push_slot(slot(
            Box::new(
                Text::new(&by)
                    .with_role(TextRole::Caption)
                    .with_id("tilolang-ask-by"),
            ),
            Fit::hug(),
        ));
        let answer = |id: &str, key: &str, role: ButtonRole, to: Answer| {
            let answers = Rc::clone(&self.inbox.answers);
            Box::new(
                Button::new(id, &t(key))
                    .role(role)
                    .on_view(move || answers.borrow_mut().push(to)),
            ) as Element
        };
        let choices = Row::row()
            .fill_width()
            .push_slot(slot(
                answer(
                    "tilolang-ask-confirm",
                    "tilolang-ask-confirm",
                    ButtonRole::Primary,
                    Answer::Allow,
                ),
                Fit::hug(),
            ))
            .push_slot(slot(
                answer(
                    "tilolang-ask-decline",
                    "tilolang-ask-decline",
                    ButtonRole::Secondary,
                    Answer::Decline,
                ),
                Fit::hug(),
            ))
            .push_slot(slot(
                answer(
                    "tilolang-ask-always",
                    "tilolang-ask-always",
                    ButtonRole::Plain,
                    Answer::Always,
                ),
                Fit::hug(),
            ));
        Box::new(bar.push_slot(slot(Box::new(choices), Fit::hug())))
    }

    fn home(&self) -> Element {
        let packages = self
            .current_host()
            .map(|host| host.packages())
            .unwrap_or_default();
        let mut cards = Grid::auto(Length::Rem(14.0)).with_id("tilolang-apps");
        for package in &packages {
            let nav = Rc::clone(&self.inbox.nav);
            let app = package.meta.app.clone();
            let card = Button::new(&format!("open-{}", package.meta.app), &package.meta.title)
                .on_view(move || nav.borrow_mut().push(Nav::Open(app.clone())));
            cards = cards.push(Box::new(card));
        }
        let heading = Text::new(&t("tilolang-home-heading"))
            .with_role(TextRole::Title)
            .with_id("tilolang-home-heading");
        let mut page = PageLayout::new()
            .with_id("tilolang-home")
            .push(Box::new(heading));
        if packages.is_empty() {
            page = page.push(Box::new(
                Text::new(&t("tilolang-home-empty")).with_role(TextRole::Body),
            ));
        } else {
            page = page.push(Box::new(cards));
        }
        Box::new(page)
    }

    // ----- navigation ----------------------------------------------------------------------

    fn show_home(&mut self) {
        // An installed app is its package: its home is its root's window.
        if let Some(app) = self.runs.as_ref().map(|bundle| bundle.meta.app.clone()) {
            self.open(&app);
            return;
        }
        self.mode = Mode::Home;
        self.body_tree = None;
        self.navigation = None;
        self.detail_shown = false;
        self.class = None;
        self.last_size = None;
        let home = self.home();
        self.set_slot(1, home, Fit::fill(1.0));
        self.rebuild_header();
    }

    fn open(&mut self, app: &str) {
        let Some(host) = self.current_host() else {
            return;
        };
        match host.open_main(app) {
            Ok(view) => {
                let title = host
                    .packages()
                    .into_iter()
                    .find(|package| package.meta.app == app)
                    .map(|package| package.meta.title)
                    .unwrap_or_default();
                self.mode = Mode::Open {
                    app: app.to_owned(),
                    title,
                    view: Box::new(view),
                    trail: Vec::new(),
                };
                self.show_view();
            }
            Err(error) => {
                tracing::warn!(app, %error, "an app did not open");
                self.notice = Some(t("tilolang-open-failed"));
                self.rebuild_chrome();
            }
        }
    }

    /// Build the body from the open view's whole tree.
    fn show_view(&mut self) {
        let Mode::Open { view, .. } = &self.mode else {
            return;
        };
        let tree = crate::reconcile::normalize(view.tree());
        self.class = None;
        self.last_size = None;
        self.reconciler.clear_navigation();
        let body = self.reconciler.build(&tree);
        self.body_tree = Some(tree);
        self.navigation = self.reconciler.navigation();
        self.detail_shown = false;
        self.pending_detail = false;
        self.set_slot(1, scrolling(body), Fit::fill(1.0));
        self.rebuild_header();
        self.commit_planned();
        self.refresh_bound();
    }

    /// Go to the window of `target` (what `open(r)` asked for): shown here, in place of the
    /// open view, which back returns to.
    fn open_resource(&mut self, target: &tilolang_tilo::runtime::ResourceRef, with: Option<&str>) {
        let Some(host) = self.current_host() else {
            return;
        };
        match host.open_resource(target, with) {
            Ok(next) => {
                if let Mode::Open { view, trail, .. } = &mut self.mode {
                    let left = std::mem::replace(view, Box::new(next));
                    trail.push(*left);
                }
                self.show_view();
            }
            Err(error) => {
                tracing::warn!(%error, "a resource did not open");
                self.notice = Some(t("tilolang-open-failed"));
                self.rebuild_chrome();
            }
        }
    }

    /// Back: to the view an `open(r)` left, or home.
    fn back(&mut self) {
        // A split showing its detail alone goes back to its list first.
        if let Some(navigation) = &self.navigation
            && navigation.detail_alone()
        {
            navigation.show(Pane::Primary);
            self.structure_dirty = true;
            return;
        }
        let previous = match &mut self.mode {
            Mode::Open { view, trail, .. } => {
                trail.pop().map(|p| std::mem::replace(view, Box::new(p)))
            }
            Mode::Home => None,
        };
        if previous.is_some() {
            if let Mode::Open { view, .. } = &mut self.mode {
                // It missed what changed while it was away.
                let _ = view.poll_patches();
            }
            self.show_view();
        } else {
            self.show_home();
        }
    }

    /// Windows the open view asked to open.
    fn follow_opens(&mut self) {
        let opens = match &mut self.mode {
            Mode::Open { view, .. } => view.poll_events(),
            Mode::Home => Vec::new(),
        };
        for event in opens {
            match event {
                ViewEvent::Open { target, view } => self.open_resource(&target, view.as_deref()),
            }
        }
    }

    // ----- the view ------------------------------------------------------------------------

    /// Re-run the open view when something it read changed and reconcile the new tree.
    fn poll_view(&mut self) -> bool {
        let Mode::Open { view, .. } = &mut self.mode else {
            return false;
        };
        match view.poll_patches() {
            Ok(patches) if patches.is_empty() => false,
            Ok(_) => {
                let new_tree = crate::reconcile::normalize(view.tree());
                self.reconcile(&new_tree);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "a view failed to run");
                false
            }
        }
    }

    fn reconcile(&mut self, new_tree: &UiNode) {
        let Some(old_tree) = self.body_tree.take() else {
            return;
        };
        if let Some(body) = self.take_body() {
            let inner = unscroll(body);
            let updated = self.reconciler.update(inner, &old_tree, new_tree);
            self.set_slot(1, scrolling(updated), Fit::fill(1.0));
        }
        if std::mem::take(&mut self.pending_detail)
            && secondary_of(&old_tree) != secondary_of(new_tree)
        {
            self.show_detail();
        }
        self.body_tree = Some(new_tree.clone());
        self.refresh_bound();
    }

    /// A press in the list pane of a view that shows one pane at a time: a row of the list opens
    /// its detail at once; any other press goes there when it changes what the detail shows.
    fn opening_detail(&mut self, key: &str) {
        let Some(navigation) = &self.navigation else {
            return;
        };
        if !navigation.collapsed() || navigation.shown() != Pane::Primary {
            return;
        }
        let Some(tree) = &self.body_tree else {
            return;
        };
        let Some(list) = primary_of(tree) else {
            return;
        };
        let Some(pressed) = list.find(key) else {
            return;
        };
        if pressed.kind == "list_row" {
            self.show_detail();
        } else {
            self.pending_detail = true;
        }
    }

    fn show_detail(&mut self) {
        if let Some(navigation) = &self.navigation {
            navigation.show(Pane::Secondary);
            self.structure_dirty = true;
        }
    }

    /// Show what the bound fields and view state hold now in the controls that edit them.
    fn refresh_bound(&self) {
        let (Mode::Open { view, .. }, Some(tree)) = (&self.mode, self.body_tree.as_ref()) else {
            return;
        };
        visit_bound(self, view, tree);
    }

    /// What the door asked of the host since the last frame: the commits it made go to the door
    /// like a press's, and a flow it left waiting for a person is asked about.
    fn take_door_work(&mut self) -> bool {
        if !self.door_work.take() {
            return false;
        }
        self.commit_planned();
        if let Some(host) = self.current_host()
            && let Some(suggestion) = host.take_suspended()
        {
            self.ask = Some(suggestion);
            self.rebuild_chrome();
        }
        true
    }

    /// Open what search hits (or an agent) asked the window to show: the app, with the resource
    /// selected in it by the app's own `open…` actions.
    fn take_open_requests(&mut self) -> bool {
        let anchors = std::mem::take(&mut *self.open_requests.borrow_mut());
        if anchors.is_empty() {
            return false;
        }
        let Some(host) = self.current_host() else {
            return false;
        };
        for anchor in anchors {
            match host.open_plan(&anchor) {
                Ok(plan) => {
                    let showing = matches!(&self.mode, Mode::Open { app, .. } if *app == plan.app);
                    if !showing {
                        self.open(&plan.app);
                    }
                    if let Some(target) = plan.target {
                        self.open_resource(&target, None);
                    }
                    self.commit_planned();
                }
                Err(error) => {
                    tracing::warn!(%error, "a search hit named nothing in the room");
                    self.notice = Some(t("tilolang-open-failed"));
                    self.rebuild_chrome();
                }
            }
        }
        true
    }

    /// Carry out the outside effects of this device that wait in the record.
    fn settle_outbox(&mut self) -> bool {
        if !std::mem::take(&mut self.settle_due) {
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

    /// Hand everything the runtime committed to the door.
    fn commit_planned(&mut self) {
        let Some(host) = self.current_host() else {
            return;
        };
        queue_commits(&host, &self.outgoing, 0, None);
        self.settle_due = true;
    }

    // ----- what the person did -------------------------------------------------------------

    fn handle_events(&mut self) -> bool {
        let mut changed = false;
        let navs = std::mem::take(&mut *self.inbox.nav.borrow_mut());
        for nav in navs {
            changed = true;
            match nav {
                Nav::Open(app) => self.open(&app),
                Nav::Home => self.show_home(),
                Nav::Back => self.back(),
            }
        }
        let answers = std::mem::take(&mut *self.inbox.answers.borrow_mut());
        for answer in answers {
            changed = true;
            self.answer(answer);
        }
        let events = std::mem::take(&mut *self.events.borrow_mut());
        for event in events {
            changed = true;
            self.event(&event);
        }
        changed
    }

    fn event(&mut self, event: &UiEvent) {
        if let UiEvent::Press(key) = event {
            self.opening_detail(key);
        }
        let Mode::Open { view, .. } = &self.mode else {
            return;
        };
        let outcome = match event {
            UiEvent::Press(key) => match view.press(key) {
                Ok(InvokeOutcome::Done(_)) => Ok(()),
                Ok(InvokeOutcome::Suggested(suggestion)) => {
                    self.ask = Some(suggestion);
                    self.rebuild_chrome();
                    Ok(())
                }
                Ok(InvokeOutcome::Denied) => Err(t("tilolang-ask-denied")),
                Err(error) => Err(error.to_string()),
            },
            UiEvent::Edit(key, text) => self.edit(key, Value::Text(text.clone())),
            UiEvent::Toggle(key, checked) => self.edit(key, Value::Bool(*checked)),
            UiEvent::Resolve(key, approve) => self.resolve(key, *approve),
        };
        match outcome {
            Ok(()) => {
                if self.notice.take().is_some() {
                    self.rebuild_chrome();
                }
            }
            Err(message) => {
                self.notice = Some(message);
                self.rebuild_chrome();
                // What the control shows is what the field holds, not what was refused.
                self.refresh_bound();
            }
        }
        self.commit_planned();
        self.follow_opens();
    }

    /// Allow or decline the suggestion an invocation card shows, as this person.
    fn resolve(&self, key: &str, approve: bool) -> Result<(), String> {
        let (Mode::Open { view, .. }, Some(host)) = (&self.mode, self.current_host()) else {
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

    fn edit(&self, key: &str, value: Value) -> Result<(), String> {
        let Mode::Open { view, .. } = &self.mode else {
            return Ok(());
        };
        let bound_to_number = view.bound_value(key);
        let value = match (&value, bound_to_number) {
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

    fn answer(&mut self, answer: Answer) {
        let (Some(suggestion), Some(host)) = (self.ask.take(), self.current_host()) else {
            return;
        };
        let resolution = match answer {
            Answer::Decline => Resolution::Decline,
            Answer::Allow | Answer::Always => {
                if answer == Answer::Always {
                    // Trust the code that asked: it is the author at the head of the chain.
                    for author in &suggestion.chain {
                        host.authority().trust(author);
                    }
                    // And what the agent asked for: agents may do it from now on.
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
            self.notice = Some(error.to_string());
        }
        self.rebuild_chrome();
        self.commit_planned();
    }

    // ----- space ---------------------------------------------------------------------------

    /// Tell the view how much room the body was offered, so a view that read `space` re-runs
    /// when a class boundary it read was crossed.
    fn feed_space(&mut self, width: f32, height: f32) {
        let Mode::Open { view, .. } = &mut self.mode else {
            return;
        };
        if self.last_size == Some((width, height)) {
            return;
        }
        self.last_size = Some((width, height));
        let offered = Space::offered(width, height, self.rem, self.class);
        self.class = Some(offered.width_class);
        // Hold the width inside its class band, so the runtime's own class agrees with the one
        // that has hysteresis.
        let (low, high) = offered.width_class.band();
        let held = offered.width.clamp(low.max(0.0), (high - 0.001).max(0.0));
        if view.set_space(
            "",
            tilolang_tilo::runtime::Space::from_rem(f64::from(held), f64::from(offered.height)),
        ) {
            self.structure_dirty = true;
        }
    }

    /// The resources the window publishes: the host's own, and every record of every package.
    fn published(&self, doc: &ui::automerge::AutoCommit) -> Vec<Resource> {
        let host = self.host_for(doc);
        let handle = Arc::new(host.handle().with_caller(|| {
            ui::invocation_scope::current_invoker().map(|invoker| caller_of(&invoker))
        }));
        let planner: Arc<dyn Planner> = handle.clone();
        let committer: Arc<dyn Committer> = handle;
        let mut out = vec![host_resource(&committer)];
        for (package, type_name, doc_id) in host.records() {
            let label = host.label_of(&doc_id);
            out.push(match host.store().record(&doc_id) {
                Ok(record) => publish_tree(
                    &package.meta,
                    &record,
                    &type_name,
                    &doc_id,
                    &label,
                    &planner,
                ),
                Err(_) => publish_record(&package.meta, &type_name, &doc_id, &label, &planner),
            });
        }
        for package in host.packages() {
            let resource = publish_package(&package.meta, &planner);
            if !resource.actions.is_empty() {
                out.push(resource);
            }
        }
        out
    }
}

fn visit_bound(page: &TilolangPage, view: &ViewInstance, node: &UiNode) {
    if node.binding.is_some()
        && let Some(value) = view.bound_value(&node.key)
    {
        match value {
            Value::Bool(flag) => page.reconciler.set_flag(&node.key, flag),
            Value::Int(0) if node.kind == "checkbox" => {
                page.reconciler.set_flag(&node.key, false);
            }
            Value::Int(1) if node.kind == "checkbox" => {
                page.reconciler.set_flag(&node.key, true);
            }
            other => page
                .reconciler
                .set_text(&node.key, &crate::reconcile::display(&other)),
        }
    }
    for child in &node.children {
        visit_bound(page, view, child);
    }
}

/// The first split of a view, which is where its list and its detail are.
fn split_of(tree: &UiNode) -> Option<&UiNode> {
    if tree.kind == "split" {
        return Some(tree);
    }
    tree.children.iter().find_map(split_of)
}

/// The list pane of the view's split.
fn primary_of(tree: &UiNode) -> Option<&UiNode> {
    split_of(tree)?
        .children
        .iter()
        .find(|pane| pane.kind == "primary")
}

/// The detail pane of the view's split.
fn secondary_of(tree: &UiNode) -> Option<&UiNode> {
    split_of(tree)?
        .children
        .iter()
        .find(|pane| pane.kind == "secondary")
}

/// The body in a vertical scroll: a view taller than its window scrolls instead of clipping.
fn scrolling(body: Element) -> Element {
    Box::new(ui::widget::Scrollable::new_vertical().with_child(body))
}

/// The body out of its scroll, to reconcile in place.
fn unscroll(mut scroll: Element) -> Element {
    if let Some(scrollable) = scroll
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<ui::widget::Scrollable>())
        && let Some(content) = scrollable.child_mut()
    {
        return std::mem::replace(content, Box::new(Spacer::fixed(0.0)));
    }
    scroll
}

impl ui::resources::PublishesResources for TilolangPage {
    fn resources(&self, doc: &ui::automerge::AutoCommit) -> Vec<Resource> {
        self.published(doc)
    }
}

impl ui::widget::controls::ActionControls for TilolangPage {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.take_attributed_control_invocation()
            .map(|(invocation, _)| invocation)
    }

    fn take_attributed_control_invocation(
        &mut self,
    ) -> Option<ui::widget::controls::AttributedControlInvocation> {
        self.take_door_work();
        // What the controls of the view queued (a card, a button) is theirs to hand over as it is;
        // what the window itself queued is a commit of the runtime's.
        let Some((invocation, origin)) = self.outgoing.borrow_mut().pop_front() else {
            return ui::widget::controls::take_attributed_control_invocation(self.root.as_mut());
        };
        let ticket = invocation
            .arguments
            .get("ticket")
            .and_then(Json::as_u64)
            .unwrap_or_default();
        self.in_flight.push_back(ticket);
        Some((invocation, origin))
    }

    fn on_control_invocation_result(&mut self, address: &ActionAddress, result: &ControlResult) {
        if address.resource == HOST_RESOURCE {
            // Answered, one way or the other: the queued action is the door's now, or dropped.
            self.door_answered(result.as_ref().err().map(ToString::to_string));
        } else {
            for child in ui::widget::Widget::children_mut(self) {
                ui::widget::controls::on_control_invocation_result(child, address, result);
            }
        }
    }
}

impl ui::search::capability::SearchSource for TilolangPage {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search::Provider::over(self.search.clone()))]
    }

    fn search_index_state(&self) -> Option<ui::search::index_publication::IndexState> {
        Some(ui::search::index_publication::IndexState {
            generation: self.search.stats().generation,
            secrets_unlocked: None,
        })
    }

    fn search_index_entries(&self) -> Vec<ui::search::index_publication::IndexEntry> {
        self.index.borrow().clone()
    }
}

impl AppWidget for TilolangPage {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }

    fn app_title(&self) -> &str {
        self.runs
            .as_ref()
            .map_or("tilolang", |bundle| bundle.meta.title.as_str())
    }

    fn app_icon(&self) -> IconDescriptor {
        let icon = self
            .runs
            .as_ref()
            .and_then(|bundle| bundle.meta.icon.as_deref())
            .unwrap_or("code-editor");
        IconDescriptor::Named(icon.replace('_', "-"))
    }
}

impl Widget for TilolangPage {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn name(&self) -> &'static str {
        "TilolangPage"
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.root.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![self.root.as_mut()]
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (doc, _) = ui::doc::document_ref(state, path);
        let host = self.host_for(doc);
        if !self.started {
            self.started = true;
            self.show_home();
            self.rebuild_chrome();
        }
        let moved = host.sync_from(doc) | self.moved.take();
        if moved {
            self.refresh_bound();
            *self.index.borrow_mut() = crate::search::refresh(&self.search, &host);
            self.settle_due = true;
        }
        if self.poll_view() {
            self.structure_dirty = true;
        }
        self.root.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let staging = self.play_stage();
        let door = self.take_door_work();
        let opened = self.take_open_requests();
        let settled = self.settle_outbox();
        let detail = self
            .navigation
            .as_ref()
            .is_some_and(Navigation::detail_alone);
        if detail != self.detail_shown {
            self.detail_shown = detail;
            self.rebuild_header();
        }
        if self.names_changed.take()
            && let Mode::Open { view, .. } = &self.mode
        {
            view.refresh();
        }
        let changed = self.handle_events() || staging || door || opened || settled;
        let child = self.root.update(state, path);
        changed || child
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.root.handle_event(event, ctx);
    }

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.rem = rem_of(theme);
        self.root.build_layout(tree, theme, nodes)
    }

    fn apply_layout(
        &mut self,
        tree: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.root.apply_layout(tree, node, origin);
        let body = self
            .root
            .children()
            .get(1)
            .map(|body| body.get_rect())
            .filter(|rect| rect.width() > 0.0);
        if let Some(rect) = body {
            #[allow(clippy::cast_possible_truncation, reason = "window sizes fit f32")]
            self.feed_space(rect.width() as f32, rect.height() as f32);
        }
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.root.get_rect()
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
        self.root.draw(scene, transform, text, theme, presence);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.structure_dirty);
        self.root.take_structure_dirty() || dirty
    }
}

/// The state a [`LocalState`] holds, for tests.
#[allow(
    dead_code,
    reason = "keeps the toolkit's state type in this module's vocabulary"
)]
type Shown = LocalState<String>;

#[cfg(test)]
mod tests {
    use tilolang_tilo::action::register_actions;
    use tilolang_tilo::runtime::MemorySink;
    use ui::automerge::AutoCommit;
    use ui::resources::PublishesResources as _;
    use ui::tilo_actions::{
        ActionContext, ActionInvoker, ActionRegistry, EncodedAction, apply_local_with_context,
    };
    use ui::widget::controls::ActionControls as _;

    use super::*;

    /// A window over a room record, driven the way the host drives it, with the door played by
    /// the test: what the window hands over is built, applied to the record and answered.
    struct Room {
        doc: AutoCommit,
        page: TilolangPage,
        /// Where the window's settled outside effects go.
        sink: MemorySink,
    }

    impl Room {
        fn new() -> Self {
            let doc = AutoCommit::new();
            let sink = MemorySink::new();
            let mut page = TilolangPage::new("ada".to_owned()).with_sink(sink.clone());
            page.sync(&doc, &ui::automerge::ROOT);
            Self { doc, page, sink }
        }

        fn frame(&mut self) {
            let (doc, root) = (&self.doc, ui::automerge::ROOT);
            self.page.sync(doc, &root);
            self.page.update(doc, &root);
        }

        /// Hand every pending invocation to the door and apply what it builds.
        fn door(&mut self) {
            while let Some((invocation, origin)) = self.page.take_attributed_control_invocation() {
                // What an effect of the door committed is applied for the task that invoked it.
                let invoker =
                    origin.map_or_else(|| ActionInvoker::actor("ada".to_owned()), |_| task());
                let resources = self.page.resources(&self.doc);
                let resource = resources
                    .iter()
                    .find(|resource| resource.id.as_str() == invocation.address.resource)
                    .expect("the host resource is published");
                let spec = resource
                    .actions
                    .iter()
                    .find(|spec| spec.id == invocation.address.action)
                    .expect("the action is published");
                let encoded: EncodedAction = spec.build_json(&invocation.arguments).unwrap();
                let action = ActionRegistry::with(register_actions)
                    .decode(&encoded)
                    .unwrap();
                let context = ActionContext::for_resource("room", "tl.room", "room")
                    .with_invoker(invoker.clone());
                apply_local_with_context(
                    &mut self.doc,
                    action.as_ref(),
                    encoded,
                    Some(invoker.actor.as_bytes()),
                    context,
                )
                .unwrap();
                self.doc.commit();
                // The door answered: the queued action is the record's now.
                if let (Some(ticket), Some(host)) =
                    (self.page.in_flight.pop_front(), self.page.current_host())
                {
                    let _ = host.store().take_ticket(ticket);
                }
            }
        }

        /// A room whose scenario `name` has been staged and committed.
        fn staged(name: &str) -> Self {
            let sink = MemorySink::new();
            let mut room = Self {
                doc: AutoCommit::new(),
                page: TilolangPage::staged("ada".to_owned(), name).with_sink(sink.clone()),
                sink,
            };
            for _ in 0..6 {
                room.frame();
                room.door();
            }
            room
        }

        /// What an agent sees of the door: every operation the window publishes, as inert data.
        fn surveyed(&self) -> Vec<ui::tilo_actions::door::ActionDescriptor> {
            ui::tilo_actions::door::survey(&self.page.resources(&self.doc))
        }

        /// Invoke `action` of `resource` the way an agent does: find it in the survey, encode its
        /// arguments against the published parameters and hand the operation to the window.
        fn agent_invokes(
            &mut self,
            resource: &str,
            action: &str,
            arguments: &Json,
        ) -> Result<ui::tilo_actions::ActionOutput, String> {
            self.invokes_as(Some(task()), resource, action, arguments)
        }

        /// As [`Self::agent_invokes`], for `who`: the person at the window when `None`.
        fn invokes_as(
            &mut self,
            who: Option<ActionInvoker>,
            resource: &str,
            action: &str,
            arguments: &Json,
        ) -> Result<ui::tilo_actions::ActionOutput, String> {
            use ui::framework::{Effected, HostDelivery as _};
            let descriptor = self
                .surveyed()
                .into_iter()
                .find(|d| d.address.resource == resource && d.address.action == action)
                .ok_or_else(|| format!("{resource}/{action} is not published"))?;
            let encoded = ui::tilo_actions::door::encode_json(
                &self.page.resources(&self.doc),
                &descriptor.address,
                &arguments.as_object().cloned().unwrap_or_default(),
            )
            .map_err(|error| error.to_string())?;
            let started = {
                let _admitted = who.map(ui::invocation_scope::InvokerScope::enter);
                self.page.start_encoded(
                    descriptor.kind,
                    &encoded.type_id,
                    &encoded.payload,
                    &mut self.doc,
                    &ui::automerge::ROOT,
                )?
            }
            .ok_or_else(|| "nothing answered the operation".to_owned())?;
            self.settle();
            match started {
                Effected::Done(output) => Ok(output),
                Effected::Pending => Err("the operation is still running".to_owned()),
            }
        }

        /// Frames and doors until the window has nothing more to hand over.
        fn settle(&mut self) {
            for _ in 0..4 {
                self.frame();
                self.door();
            }
            self.frame();
        }

        /// Apply a published verb as the door does for `who`: encode it (its planner reads the
        /// admitted invoker), then commit it attributed to them.
        fn applies_as(
            &mut self,
            who: Option<ActionInvoker>,
            resource: &str,
            action: &str,
            arguments: &Json,
        ) -> Result<(), String> {
            let address = ui::tilo_actions::door::published(
                &self.page.resources(&self.doc),
                resource,
                action,
            )
            .ok_or_else(|| format!("{resource}/{action} is not published"))?;
            let invoker = who
                .clone()
                .unwrap_or_else(|| ActionInvoker::actor("ada".to_owned()));
            let encoded = {
                let _admitted = who.map(ui::invocation_scope::InvokerScope::enter);
                ui::tilo_actions::door::encode_json(
                    &self.page.resources(&self.doc),
                    &address,
                    &arguments.as_object().cloned().unwrap_or_default(),
                )
                .map_err(|error| error.to_string())?
            };
            let action = ActionRegistry::with(register_actions)
                .decode(&encoded)
                .map_err(|error| error.to_string())?;
            let context =
                ActionContext::for_resource(resource, "tl", resource).with_invoker(invoker.clone());
            apply_local_with_context(
                &mut self.doc,
                action.as_ref(),
                encoded,
                Some(invoker.actor.as_bytes()),
                context,
            )
            .map_err(|error| error.to_string())?;
            self.doc.commit();
            self.settle();
            Ok(())
        }

        fn open(&mut self, app: &str) {
            self.page
                .inbox
                .nav
                .borrow_mut()
                .push(Nav::Open(app.to_owned()));
            self.frame();
            self.door();
            self.frame();
        }

        fn shown_count(&self) -> String {
            let tree = self.page.body_tree.as_ref().expect("a view is open");
            first_text(tree).unwrap_or_default()
        }

        fn button(&self, label: &str) -> String {
            labelled_button(self.page.body_tree.as_ref().expect("a view is open"), label)
                .expect("a button")
        }
    }

    fn first_text(node: &UiNode) -> Option<String> {
        // The counter shows its value as a heading.
        if node.kind == "text" || node.kind == "heading" {
            return node.props.get("value").map(crate::reconcile::display);
        }
        node.children.iter().find_map(first_text)
    }

    fn rows(node: &UiNode, out: &mut Vec<String>) {
        if node.kind == "list_row" {
            out.push(
                node.props
                    .get("title")
                    .map(crate::reconcile::display)
                    .unwrap_or_default(),
            );
        }
        for child in &node.children {
            rows(child, out);
        }
    }

    fn labelled_button(node: &UiNode, label: &str) -> Option<String> {
        if node.kind == "button"
            && node.props.get("label") == Some(&tilolang_tilo::runtime::Value::text(label))
        {
            return Some(node.key.clone());
        }
        node.children
            .iter()
            .find_map(|child| labelled_button(child, label))
    }

    /// The control labelled `label`: a button by its label, a row of a list by its title.
    fn button_labelled(node: &UiNode, label: &str) -> Option<String> {
        let named = match node.kind.as_str() {
            "button" => node.props.get("label") == Some(&Value::text(label)),
            "list_row" => node.props.get("title") == Some(&Value::text(label)),
            _ => false,
        };
        if named {
            return Some(node.key.clone());
        }
        node.children
            .iter()
            .find_map(|child| button_labelled(child, label))
    }

    fn has_placeholder(node: &UiNode, placeholder: &str) -> bool {
        node.props.get("placeholder") == Some(&Value::text(placeholder))
            || node
                .children
                .iter()
                .any(|child| has_placeholder(child, placeholder))
    }

    impl Room {
        fn press(&mut self, label: &str) {
            let tree = self.page.body_tree.as_ref().expect("a view is open");
            let key = button_labelled(tree, label)
                .unwrap_or_else(|| panic!("no button {label} in {tree:#?}"));
            self.page.act(UiEvent::Press(key));
            self.frame();
            self.door();
            self.frame();
        }
    }

    // USER REQUIREMENT (2026-10-20): `open(r)` "the host minimally implements it as
    // push-navigation to the resource's default view with back".
    #[test]
    fn a_notes_row_opens_the_note_in_place_and_back_returns_to_the_list() {
        let mut room = Room::new();
        room.open("so.tilo.lang.notes");
        room.press("New note");
        room.press("Untitled");
        let shown = room.page.shown().expect("a view is open");
        assert!(
            has_placeholder(shown, "Title"),
            "the note's editor: {shown:#?}"
        );

        let title = {
            fn find(node: &UiNode) -> Option<String> {
                if node.props.get("placeholder") == Some(&Value::text("Title")) {
                    return Some(node.key.clone());
                }
                node.children.iter().find_map(find)
            }
            find(shown).expect("the title input")
        };
        room.page.act(UiEvent::Edit(title, "Plans".to_owned()));
        room.frame();
        room.door();
        room.frame();

        room.page.inbox.nav.borrow_mut().push(Nav::Back);
        room.frame();
        let list = room.page.shown().expect("the notebook again");
        assert!(!has_placeholder(list, "Title"));
        assert!(
            button_labelled(list, "Plans").is_some(),
            "the list shows what the editor changed: {list:#?}"
        );

        room.page.inbox.nav.borrow_mut().push(Nav::Back);
        room.frame();
        assert_eq!(room.page.open_app(), None, "back from the root goes home");
    }

    #[test]
    fn the_home_lists_the_built_in_packages() {
        let room = Room::new();
        let host = room.page.current_host().expect("the host started");
        let apps: Vec<String> = host.packages().into_iter().map(|p| p.meta.app).collect();
        assert!(
            apps.contains(&"so.tilo.lang.counter".to_owned()),
            "{apps:?}"
        );
        assert!(apps.contains(&"so.tilo.lang.notes".to_owned()), "{apps:?}");
    }

    #[test]
    fn opening_an_app_creates_its_root_through_the_door_and_shows_it() {
        let mut room = Room::new();
        room.open("so.tilo.lang.counter");
        assert_eq!(room.shown_count(), "0");
        // The root exists in the record now, committed as a recorded action.
        let records = tilolang_tilo::action::resource_ids(&room.doc, "so.tilo.lang.counter");
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn a_press_counts_through_the_runtime_and_the_door() {
        let mut room = Room::new();
        room.open("so.tilo.lang.counter");
        let key = room.button("+1");
        for expected in ["1", "2", "3"] {
            room.page
                .events
                .borrow_mut()
                .push(UiEvent::Press(key.clone()));
            room.frame();
            room.door();
            room.frame();
            assert_eq!(room.shown_count(), expected);
        }
    }

    #[test]
    fn the_published_tree_has_the_host_and_every_record() {
        let mut room = Room::new();
        room.open("so.tilo.lang.counter");
        let resources = room.page.resources(&room.doc);
        let kinds: Vec<&str> = resources.iter().map(|r| r.kind.as_str()).collect();
        assert!(kinds.contains(&"tl.host"), "{kinds:?}");
        assert!(
            kinds.contains(&"tl.so.tilo.lang.counter.Counter"),
            "{kinds:?}"
        );
    }

    #[test]
    #[allow(clippy::similar_names, reason = "the room and the notebook's root")]
    fn the_notes_package_opens_and_a_new_note_is_created_through_the_door() {
        let mut room = Room::new();
        room.open("so.tilo.lang.notes");
        let host = room.page.current_host().expect("the host started");
        // The notes are items of the notebook's collection, not resources of their own.
        let count_notes = |host: &Host| {
            let (package, _, notebook) = host
                .records()
                .into_iter()
                .find(|(_, ty, _)| ty == "Notebook")
                .expect("the notebook exists");
            let field = package
                .meta
                .types
                .iter()
                .find(|declared| declared.name == "Notebook")
                .and_then(|declared| declared.fields.iter().find(|field| field.name == "notes"))
                .map(|field| field.id.clone())
                .expect("the notebook has notes");
            match host
                .store()
                .record(&notebook)
                .expect("the notebook reads")
                .get(&field)
            {
                Some(tilolang_tilo::DynValue::Collection(items)) => items.len(),
                other => panic!("not a collection: {other:?}"),
            }
        };
        assert_eq!(count_notes(&host), 0, "no notes yet");
        let key = room.button("New note");
        room.page.events.borrow_mut().push(UiEvent::Press(key));
        room.frame();
        room.door();
        room.frame();
        assert_eq!(count_notes(&host), 1, "the press inserted a note");
    }

    #[test]
    #[allow(
        clippy::similar_names,
        reason = "the package and the pack of notes it holds"
    )]
    fn a_staged_scenario_adds_exactly_its_notes() {
        let room = Room::staged("notes");
        let host = room.page.current_host().expect("the host started");
        let (package, _, root) = host
            .records()
            .into_iter()
            .find(|(_, ty, _)| ty == "Notebook")
            .expect("the notebook exists");
        let field = package
            .meta
            .types
            .iter()
            .find(|declared| declared.name == "Notebook")
            .and_then(|declared| declared.fields.iter().find(|field| field.name == "notes"))
            .map(|field| field.id.clone())
            .expect("the notebook has notes");
        let on_disk = tilolang_tilo::action::read_resource(&room.doc, "so.tilo.lang.notes", &root)
            .expect("the record has it");
        let Some(tilolang_tilo::DynValue::Collection(stored)) = on_disk.get(&field) else {
            panic!("not a collection");
        };
        assert_eq!(stored.len(), 3, "three notes, however many frames ran");
        // And the open view lists exactly those three.
        let tree = room
            .page
            .body_tree
            .as_ref()
            .expect("the notes view is open");
        let mut notes = Vec::new();
        rows(tree, &mut notes);
        assert_eq!(notes.len(), 3, "{notes:?}");
    }

    const HOST: &str = "tl:host";

    /// The delegate of an agent's task, acting for the person at the window.
    fn task() -> ActionInvoker {
        ActionInvoker::actor("agent-task:7")
            .with_kind(ui::tilo_actions::ActorKind::Agent)
            .acting_for("ada")
    }

    #[test]
    fn an_agent_finds_the_hosts_operations_in_the_survey_and_reads_the_catalogue() {
        let mut room = Room::staged("chat");
        let host_actions: Vec<String> = room
            .surveyed()
            .into_iter()
            .filter(|d| d.address.resource == HOST)
            .map(|d| d.address.action)
            .collect();
        for wanted in ["run-block", "run-flow", "catalogue"] {
            assert!(host_actions.iter().any(|a| a == wanted), "{host_actions:?}");
        }
        // The typed discovery path (what `list_operations` reads) holds the packages' verbs
        // beside them, and nothing it lists lacks a handler or a decoder.
        let survey = room.surveyed();
        assert!(
            survey
                .iter()
                .any(|d| d.resource_kind == "tl.so.tilo.lang.letters.Desk"
                    && d.action_label == "new_draft"),
            "the verbs of a record are discoverable"
        );
        let resources = room.page.resources(&room.doc);
        let violations = ui::tilo_actions::validate_surface(
            &resources,
            &ActionRegistry::with(register_actions),
            &ui::widget::Widget::resource_operation_handlers(&room.page),
        );
        assert!(violations.is_empty(), "{violations:#?}");
        let output = room.agent_invokes(HOST, "catalogue", &Json::Null).unwrap();
        let signatures = output.value.expect("the catalogue is the answer");
        let text = signatures.to_string();
        assert!(text.contains("so.tilo.lang.letters"), "{text}");
        assert!(text.contains("action so.tilo.lang.letters.send"), "{text}");
        assert!(
            text.contains("Posts the letter"),
            "doc comments come with it: {text}"
        );
    }

    #[test]
    fn a_view_block_a_value_block_and_a_bad_block_answer_an_agent() {
        let mut room = Room::staged("chat");
        let run = |room: &mut Room, source: &str| {
            room.agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
        };
        let view = run(&mut room, "column { text(\"Hello from an agent\") }")
            .unwrap()
            .value
            .unwrap();
        assert_eq!(view["kind"], "view", "{view}");
        assert!(view.to_string().contains("Hello from an agent"), "{view}");
        let data = run(&mut room, "1 + 2").unwrap().value.unwrap();
        assert_eq!(data, serde_json::json!({"kind": "data", "value": 3}));
        let refused = run(&mut room, "nothing.here(").unwrap_err();
        assert!(refused.contains("does not compile"), "{refused}");
    }

    #[test]
    fn a_send_block_from_an_agent_becomes_a_suggested_invocation_a_person_decides() {
        let mut room = Room::staged("chat");
        let host = room.page.current_host().expect("the host started");
        let desk = host.root_of("so.tilo.lang.letters").unwrap();
        let tilolang_tilo::runtime::Value::List(drafts) =
            host.runtime().read(&desk, "drafts").unwrap()
        else {
            panic!("the desk lists its drafts");
        };
        let tilolang_tilo::runtime::Value::Ref(draft) = &drafts[0] else {
            panic!("a draft is a reference");
        };
        let source = format!("letters.send(@letters.Draft:{})", draft.target.id_string());
        let output = room
            .agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
            .unwrap()
            .value
            .unwrap();
        assert_eq!(output["kind"], "invocation", "{output}");
        assert_eq!(
            output["state"], "suggested",
            "an agent's send asks first: {output}"
        );
        // The invocation is a resource of the room record, so every replica shows it.
        let id = output["invocation"].as_str().unwrap();
        let invocation = host.invocation_ref(id).unwrap();
        assert_eq!(
            host.runtime().read_invocation(&invocation).unwrap().state,
            "suggested"
        );
        let in_record = tilolang_tilo::action::resource_ids(&room.doc, tilolang_tilo::HOST_APP);
        assert!(
            in_record.iter().any(|doc| doc == &invocation.doc),
            "{in_record:?}"
        );
        // A person declines it, and nothing is sent.
        host.runtime()
            .resolve_invocation(
                &invocation,
                &tilolang_tilo::runtime::Principal::new("ada"),
                tilolang_tilo::runtime::Resolution::Decline,
            )
            .unwrap();
        let decided = host.runtime().read_invocation(&invocation).unwrap();
        assert_eq!(decided.state, "declined", "{decided:?}");
    }

    #[test]
    fn an_agent_runs_a_flow_of_a_trusted_package_and_reads_what_it_returned() {
        let mut room = Room::staged("board");
        let host = room.page.current_host().expect("the host started");
        let list = host.root_of("so.tilo.lang.todo").unwrap().id_string();
        let notebook = host.root_of("so.tilo.lang.notes").unwrap().id_string();
        let output = room
            .agent_invokes(
                HOST,
                "run-flow",
                &serde_json::json!({
                    "flow": "so.tilo.lang.board.morning_cleanup",
                    "args": serde_json::json!({"list": list, "nb": notebook}).to_string(),
                }),
            )
            .unwrap()
            .value
            .unwrap();
        assert_eq!(output, serde_json::json!({"state": "done", "result": 1}));
    }

    fn person_decides(room: &mut Room, decision: &str, target: &str) -> Result<Json, String> {
        room.invokes_as(
            None,
            HOST,
            "resolve",
            &serde_json::json!({"target": target, "decision": decision}),
        )
        .map(|output| output.value.unwrap_or_default())
    }

    #[test]
    fn approving_a_pressed_send_through_the_ask_bar_delivers_the_letter_once() {
        let mut room = Room::staged("letters-detail");
        let send = room.button("Send");
        room.page.events.borrow_mut().push(UiEvent::Press(send));
        room.settle();
        assert!(room.page.ask.is_some(), "a send asks the person first");
        assert!(
            room.sink.delivered().is_empty(),
            "nothing leaves before they say so"
        );
        room.page.inbox.answers.borrow_mut().push(Answer::Allow);
        room.settle();
        let delivered = room.sink.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert_eq!(delivered[0].0, "post");
        assert!(
            delivered[0].1.to_string().contains("tomas@example.org"),
            "{delivered:?}"
        );
        room.settle();
        room.settle();
        assert_eq!(room.sink.delivered().len(), 1, "settled once, not again");
        // The record says it was delivered, so no device queues it again.
        let host = room.page.current_host().unwrap();
        assert_eq!(
            tilolang_tilo::runtime::store::Store::outbox_pending(host.store()).len(),
            0
        );
    }

    #[test]
    fn a_person_approves_an_agents_suggested_send_through_the_door_and_it_leaves_once() {
        let mut room = Room::staged("chat");
        let host = room.page.current_host().expect("the host started");
        let desk = host.root_of("so.tilo.lang.letters").unwrap();
        let tilolang_tilo::runtime::Value::List(drafts) =
            host.runtime().read(&desk, "drafts").unwrap()
        else {
            panic!("the desk lists its drafts");
        };
        let tilolang_tilo::runtime::Value::Ref(draft) = &drafts[0] else {
            panic!("a draft is a reference");
        };
        let source = format!("letters.send(@letters.Draft:{})", draft.target.id_string());
        let suggested = room
            .agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
            .unwrap()
            .value
            .unwrap();
        assert_eq!(suggested["state"], "suggested", "{suggested}");
        let id = suggested["invocation"].as_str().unwrap().to_owned();
        // The runtime knows the author as the task's delegate, not a name of the host's own.
        let record = host
            .runtime()
            .read_invocation(&host.invocation_ref(&id).unwrap())
            .unwrap();
        assert_eq!(record.author, Principal::new("agent-task:7"));
        // An agent cannot approve what it suggested.
        let refused = room.invokes_as(
            Some(task()),
            HOST,
            "resolve",
            &serde_json::json!({"target": id, "decision": "approve"}),
        );
        assert!(
            refused.unwrap_err().contains("only a person"),
            "agents never decide"
        );
        assert!(room.sink.delivered().is_empty());
        let decided = person_decides(&mut room, "approve", &id).unwrap();
        assert_eq!(decided["state"], "done", "{decided}");
        let delivered = room.sink.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert!(
            delivered[0].1.to_string().contains("maria@example.org"),
            "{delivered:?}"
        );
        // Approving again changes nothing: the suggestion is decided.
        assert!(person_decides(&mut room, "approve", &id).is_err());
        room.settle();
        assert_eq!(room.sink.delivered().len(), 1);
    }

    #[test]
    fn a_flow_an_agent_may_not_run_alone_waits_for_a_person_and_completes_once() {
        let mut room = Room::staged("board");
        let host = room.page.current_host().expect("the host started");
        // The person narrowed this agent: changes to the todo app ask first.
        host.set_agent_answer(
            "so.tilo.lang.todo",
            tilolang_tilo::runtime::meta::EffectClass::Undoable,
            Decision::Ask,
        );
        let list = host.root_of("so.tilo.lang.todo").unwrap().id_string();
        let notebook = host.root_of("so.tilo.lang.notes").unwrap().id_string();
        let waiting = room
            .agent_invokes(
                HOST,
                "run-flow",
                &serde_json::json!({
                    "flow": "so.tilo.lang.board.morning_cleanup",
                    "args": serde_json::json!({"list": list, "nb": notebook}).to_string(),
                }),
            )
            .unwrap()
            .value
            .unwrap();
        assert_eq!(waiting["state"], "suspended", "{waiting}");
        assert!(
            room.page.ask.is_some(),
            "the window asks the person, as for a press"
        );
        let todos_before = items_of(&room, "so.tilo.lang.todo", "items");
        assert_eq!(todos_before, 3, "nothing was removed while it waits");
        let run = waiting["run"].as_str().unwrap().to_owned();
        let done = person_decides(&mut room, "approve", &run).unwrap();
        assert_eq!(done["state"], "done", "{done}");
        assert_eq!(
            items_of(&room, "so.tilo.lang.todo", "items"),
            2,
            "the finished todo went, once"
        );
        let notes_after = items_of(&room, "so.tilo.lang.notes", "notes");
        assert_eq!(
            notes_after, 3,
            "and one note says so: two staged and the morning one"
        );
        assert!(
            person_decides(&mut room, "approve", &run).is_err(),
            "it completes once"
        );
        room.settle();
        assert_eq!(items_of(&room, "so.tilo.lang.todo", "items"), 2);
        assert_eq!(items_of(&room, "so.tilo.lang.notes", "notes"), 3);
    }

    /// How many items the collection `field` of the root of `app` holds.
    fn items_of(room: &Room, app: &str, field: &str) -> usize {
        let host = room.page.current_host().unwrap();
        let notebook = host.root_of(app).unwrap();
        match host.runtime().read(&notebook, field).unwrap() {
            tilolang_tilo::runtime::Value::List(items) => items.len(),
            other => panic!("not a list: {other:?}"),
        }
    }

    #[test]
    fn the_verbs_of_items_are_published_on_the_door_and_an_agent_calls_them() {
        let mut room = Room::staged("notes");
        let survey = room.surveyed();
        let pin = survey
            .iter()
            .find(|d| {
                d.resource_kind == "tl.so.tilo.lang.notes.Note" && d.action_label == "toggle_pin"
            })
            .expect("a note publishes its own verbs");
        let (resource, action) = (pin.address.resource.clone(), pin.address.action.clone());
        assert!(
            resource.contains('/'),
            "an item is addressed by its path: {resource}"
        );
        let before = pinned(&room, &resource);
        room.applies_as(Some(task()), &resource, &action, &Json::Null)
            .unwrap();
        assert_eq!(
            pinned(&room, &resource),
            !before,
            "the agent pinned the note"
        );
        // A draft's send reaches outside: asked of an agent, it is recorded as a suggestion.
        let mut chat = Room::staged("chat");
        let send = chat
            .surveyed()
            .into_iter()
            .find(|d| {
                d.resource_kind == "tl.so.tilo.lang.letters.Draft" && d.action_label == "send"
            })
            .expect("a draft publishes its send");
        chat.applies_as(
            Some(task()),
            &send.address.resource,
            &send.address.action,
            &Json::Null,
        )
        .unwrap();
        let host = chat.page.current_host().unwrap();
        let waiting = tilolang_tilo::action::resource_ids(&chat.doc, tilolang_tilo::HOST_APP);
        let states: Vec<String> = waiting
            .iter()
            .filter_map(|doc| host.invocation_ref(doc))
            .filter_map(|inv| host.runtime().read_invocation(&inv).ok())
            .map(|record| record.state)
            .collect();
        assert!(
            states.iter().any(|state| state == "suggested"),
            "{states:?}"
        );
        assert!(chat.sink.delivered().is_empty(), "nothing was sent");
    }

    /// Whether the note published as `resource` is pinned.
    fn pinned(room: &Room, resource: &str) -> bool {
        let host = room.page.current_host().unwrap();
        let path = resource.strip_prefix("tl:so.tilo.lang.notes:").unwrap();
        let probe = tilolang_tilo::runtime::ResourceRef::parse_id(path, String::new()).unwrap();
        let ty = tilolang_tilo::runtime::store::Store::type_of(host.store(), &probe).unwrap();
        let note = tilolang_tilo::runtime::ResourceRef::parse_id(path, ty).unwrap();
        matches!(
            host.runtime().read(&note, "pinned").unwrap(),
            tilolang_tilo::runtime::Value::Bool(true)
        )
    }

    #[test]
    fn search_finds_notes_and_chat_messages_and_a_hit_opens_the_host_at_the_resource() {
        use ui::search::capability::search_providers;
        let mut room = Room::staged("notes");
        let found = |room: &Room, query: &str| {
            search_providers(&room.page)
                .iter()
                .flat_map(|provider| provider.search(query))
                .collect::<Vec<_>>()
        };
        let hits = found(&room, "groceries");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].title, "Groceries");
        assert_eq!(hits[0].kind, "Notes");
        // The system's persistent index keeps the title and short fields, never a prose body.
        let entries = ui::search::capability::SearchSource::search_index_entries(&room.page);
        let groceries = entries
            .iter()
            .find(|entry| entry.title == "Groceries")
            .unwrap();
        assert_eq!(
            groceries.action.as_ref().map(|a| a.action.as_str()),
            Some("open")
        );
        assert_eq!(groceries.group.as_deref(), Some("Notes"));
        assert!(
            groceries
                .fields
                .iter()
                .all(|(text, _)| text.chars().count() <= 80),
            "{groceries:?}"
        );
        // Open it: the hit's action is the resource's own `open` view action.
        let address = hits[0]
            .action_address()
            .expect("a hit names an action of its resource");
        // Show the home first, so the hit has to open the app.
        room.page.inbox.nav.borrow_mut().push(Nav::Home);
        room.settle();
        assert!(room.page.body_tree.is_none());
        room.invokes_as(None, &address.resource, &address.action, &Json::Null)
            .unwrap();
        let tree = room
            .page
            .body_tree
            .as_ref()
            .expect("the hit opened the app");
        // The window shown is the note's own, over the note the hit named.
        let host = room.page.current_host().unwrap();
        let title_input = {
            fn find(node: &UiNode) -> Option<&UiNode> {
                if node.props.get("placeholder") == Some(&Value::text("Title")) {
                    return Some(node);
                }
                node.children.iter().find_map(find)
            }
            find(tree).expect("the note's editor")
        };
        let binding = title_input.binding.as_ref().expect("the title is bound");
        let BindTarget::Field { target, field } = &binding.target else {
            panic!("the title is a field of the note");
        };
        assert_eq!(
            host.runtime().read(target, field).unwrap(),
            tilolang_tilo::runtime::Value::text("Groceries")
        );
        // Chat messages are found too, and a hit opens its conversation.
        let chat = Room::staged("chat");
        let messages = found(&chat, "packing list");
        assert!(
            messages
                .iter()
                .any(|hit| hit.title.contains("Here is the packing list")),
            "{messages:?}"
        );
    }
}
