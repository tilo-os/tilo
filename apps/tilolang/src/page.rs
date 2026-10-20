//! The host's window: a home view listing the packages, and any package's `main` view.
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
use tilolang_tilo::HostEffect;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{
    COMMIT, Committer, HOST_RESOURCE, Planner, host_resource, publish_package, publish_record,
};
use tilolang_tilo::runtime::{
    ActionInfo, Decision, EditOutcome, InvokeOutcome, Principal, Resolution, Suggestion, UiNode,
    Value, ViewInstance,
};
use ui::LocalState;
use ui::tilo_actions::{ActionAddress, ControlInvocation, Resource};
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
use crate::packages::EMBEDDED;
use crate::reconcile::{EventSink, Reconciler, UiEvent};

/// Where the window is.
enum Mode {
    Home,
    Open {
        view: Box<ViewInstance>,
        title: String,
    },
}

/// A navigation the person asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Nav {
    Open(String),
    Home,
    /// From the detail pane of a view collapsed to one pane back to its list.
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

/// Who an agent's block on the door is written by: its code is the author, and a person decides
/// what it may do.
const AGENT_ON_THE_DOOR: &str = "agent:door";

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
    outgoing: VecDeque<ControlInvocation>,
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
    /// What the room's participants are called, and whether a frame brought a new name.
    names: Names,
    names_changed: std::cell::Cell<bool>,
    /// The split of the open view that shows one pane at a time when narrow, and whether the
    /// header offers the way back from its detail pane.
    navigation: Option<Navigation>,
    detail_shown: bool,
    /// A press in the list pane may open something in the detail pane: go there once it does.
    pending_detail: bool,
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
        // A bound control is announced by the name of the field it edits.
        let names = Rc::new(move |binding: &tilolang_tilo::runtime::Binding| {
            shared
                .borrow()
                .as_ref()
                .and_then(|host| {
                    host.packages().into_iter().find_map(|package| {
                        let declared = package
                            .meta
                            .types
                            .iter()
                            .find(|declared| declared.id == binding.target.ty)?;
                        declared
                            .fields
                            .iter()
                            .find(|field| field.id == binding.field)
                            .map(|field| field.name.clone())
                    })
                })
                .unwrap_or_default()
        });
        let door_work: Rc<std::cell::Cell<bool>> = Rc::default();
        let answering = (Rc::clone(&host), Rc::clone(&door_work));
        let endpoint = ui::framework::EffectEndpoint::new(move |effect: HostEffect| {
            let host = answering
                .0
                .borrow()
                .clone()
                .ok_or_else(|| "the tilolang host has not started".to_owned())?;
            let answered = host.answer(effect, &Principal::new(AGENT_ON_THE_DOOR));
            answering.1.set(true);
            answered
        });
        let root = Stack::stack()
            .fill_width()
            .fill_height()
            .gap(Rhythm::Tight)
            .push_slot(slot(Box::new(Spacer::fixed(0.0)), Fit::hug()))
            .push_slot(slot(Box::new(Spacer::fixed(0.0)), Fit::fill(1.0)))
            .push_slot(slot(Box::new(Spacer::fixed(0.0)), Fit::hug()))
            .push_slot(slot(Box::new(endpoint), Fit::hug()));
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
            outgoing: VecDeque::new(),
            in_flight: VecDeque::new(),
            ask: None,
            notice: None,
            class: None,
            rem: 16.0,
            last_size: None,
            started: false,
            moved: std::cell::Cell::new(false),
            door_work,
            names_changed: std::cell::Cell::new(false),
            navigation: None,
            detail_shown: false,
            pending_detail: false,
            stage: VecDeque::new(),
        }
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
        let built = Host::new(
            doc,
            Principal::new(self.me.clone()),
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
        for package in EMBEDDED {
            if let Err(error) = host.load_builtin(package.source, package.ids) {
                tracing::error!(package = package.dir, %error, "a built-in package did not load");
            }
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
            Mode::Open { ref title, .. } => {
                let nav = Rc::clone(&self.inbox.nav);
                let mut segments = vec![t("tilolang-back"), title.clone()];
                if self.detail_shown {
                    segments.push(t("tilolang-detail"));
                }
                let crumbs = Breadcrumb::new(segments).on_navigate(move |index| match index {
                    0 => nav.borrow_mut().push(Nav::Home),
                    1 => nav.borrow_mut().push(Nav::Back),
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
                let tree = crate::reconcile::normalize(view.tree());
                let title = host
                    .packages()
                    .into_iter()
                    .find(|package| package.meta.app == app)
                    .map(|package| package.meta.title)
                    .unwrap_or_default();
                self.mode = Mode::Open {
                    title,
                    view: Box::new(view),
                };
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
            Err(error) => {
                tracing::warn!(app, %error, "an app did not open");
                self.notice = Some(t("tilolang-open-failed"));
                self.rebuild_chrome();
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

    /// Show what the bound fields hold now in the controls that edit them.
    fn refresh_bound(&self) {
        let (Some(host), Some(tree)) = (self.current_host(), self.body_tree.clone()) else {
            return;
        };
        visit_bound(self, &host, &tree);
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

    /// Hand everything the runtime committed to the door.
    fn commit_planned(&mut self) {
        let Some(host) = self.current_host() else {
            return;
        };
        for committed in host.take_committed() {
            let mut arguments = serde_json::Map::new();
            arguments.insert("ticket".to_owned(), Json::from(committed.ticket));
            if let Some(invocation) =
                ControlInvocation::from_parts(HOST_RESOURCE, COMMIT, arguments)
            {
                self.outgoing.push_back(invocation);
            }
        }
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
                Nav::Back => {
                    if let Some(navigation) = &self.navigation {
                        navigation.show(Pane::Primary);
                        self.structure_dirty = true;
                    }
                }
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
        let bound_to_number = view
            .tree()
            .find(key)
            .and_then(|node| node.binding.as_ref())
            .and_then(|binding| {
                self.current_host()?
                    .runtime()
                    .read(&binding.target, &binding.field)
                    .ok()
            });
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
        let handle = Arc::new(host.handle());
        let planner: Arc<dyn Planner> = handle.clone();
        let committer: Arc<dyn Committer> = handle;
        let mut out = vec![host_resource(&committer)];
        for (package, type_name, doc_id) in host.records() {
            let label = host.label_of(&doc_id);
            out.push(publish_record(
                &package.meta,
                &type_name,
                &doc_id,
                &label,
                &planner,
            ));
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

fn visit_bound(page: &TilolangPage, host: &Host, node: &UiNode) {
    if let Some(binding) = &node.binding
        && let Ok(value) = host.runtime().read(&binding.target, &binding.field)
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
        visit_bound(page, host, child);
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
        self.take_door_work();
        let invocation = self.outgoing.pop_front()?;
        let ticket = invocation
            .arguments
            .get("ticket")
            .and_then(Json::as_u64)
            .unwrap_or_default();
        self.in_flight.push_back(ticket);
        Some(invocation)
    }

    fn on_control_invocation_result(&mut self, _address: &ActionAddress, result: &ControlResult) {
        let Some(ticket) = self.in_flight.pop_front() else {
            return;
        };
        if let Some(host) = self.current_host() {
            // Answered, one way or the other: the queued action is the door's now, or dropped.
            let _ = host.store().take_ticket(ticket);
        }
        if let Err(error) = result {
            self.notice = Some(error.to_string());
            self.rebuild_chrome();
        }
    }
}

impl AppWidget for TilolangPage {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }

    fn app_title(&self) -> &'static str {
        "tilolang"
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Named("code-editor".into())
    }
}

impl Widget for TilolangPage {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources
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
        }
        if self.poll_view() {
            self.structure_dirty = true;
        }
        self.root.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let staging = self.play_stage();
        let door = self.take_door_work();
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
        let changed = self.handle_events() || staging || door;
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
    }

    impl Room {
        fn new() -> Self {
            let doc = AutoCommit::new();
            let mut page = TilolangPage::new("ada".to_owned());
            page.sync(&doc, &ui::automerge::ROOT);
            Self { doc, page }
        }

        fn frame(&mut self) {
            let (doc, root) = (&self.doc, ui::automerge::ROOT);
            self.page.sync(doc, &root);
            self.page.update(doc, &root);
        }

        /// Hand every pending invocation to the door and apply what it builds.
        fn door(&mut self) {
            while let Some(invocation) = self.page.take_control_invocation() {
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
                    .with_invoker(ActionInvoker::actor("ada".to_owned()));
                apply_local_with_context(
                    &mut self.doc,
                    action.as_ref(),
                    encoded,
                    Some(b"ada"),
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
            let mut room = Self {
                doc: AutoCommit::new(),
                page: TilolangPage::staged("ada".to_owned(), name),
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
            let started = self
                .page
                .start_encoded(
                    descriptor.kind,
                    &encoded.type_id,
                    &encoded.payload,
                    &mut self.doc,
                    &ui::automerge::ROOT,
                )?
                .ok_or_else(|| "nothing answered the operation".to_owned())?;
            self.frame();
            self.door();
            self.frame();
            match started {
                Effected::Done(output) => Ok(output),
                Effected::Pending => Err("the operation is still running".to_owned()),
            }
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
        if node.kind == "text" {
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

    #[test]
    fn the_home_lists_the_built_in_packages() {
        let room = Room::new();
        let host = room.page.current_host().expect("the host started");
        let apps: Vec<String> = host.packages().into_iter().map(|p| p.meta.app).collect();
        assert!(apps.contains(&"so.tilo.counter".to_owned()), "{apps:?}");
        assert!(apps.contains(&"so.tilo.notes".to_owned()), "{apps:?}");
    }

    #[test]
    fn opening_an_app_creates_its_root_through_the_door_and_shows_it() {
        let mut room = Room::new();
        room.open("so.tilo.counter");
        assert_eq!(room.shown_count(), "0");
        // The root exists in the record now, committed as a recorded action.
        let records = tilolang_tilo::action::resource_ids(&room.doc, "so.tilo.counter");
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn a_press_counts_through_the_runtime_and_the_door() {
        let mut room = Room::new();
        room.open("so.tilo.counter");
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
        room.open("so.tilo.counter");
        let resources = room.page.resources(&room.doc);
        let kinds: Vec<&str> = resources.iter().map(|r| r.kind.as_str()).collect();
        assert!(kinds.contains(&"tl.host"), "{kinds:?}");
        assert!(kinds.contains(&"tl.so.tilo.counter.Counter"), "{kinds:?}");
    }

    #[test]
    #[allow(clippy::similar_names, reason = "the room and the notebook's root")]
    fn the_notes_package_opens_and_a_new_note_is_created_through_the_door() {
        let mut room = Room::new();
        room.open("so.tilo.notes");
        let host = room.page.current_host().expect("the host started");
        // The notes are items of the notebook's collection, not resources of their own.
        let count_notes = |host: &Host| {
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
            match host
                .store()
                .record(&root)
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
        let mut room = Room {
            doc: AutoCommit::new(),
            page: TilolangPage::staged("ada".to_owned(), "notes"),
        };
        for _ in 0..6 {
            room.frame();
            room.door();
        }
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
        let on_disk = tilolang_tilo::action::read_resource(&room.doc, "so.tilo.notes", &root)
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
                .any(|d| d.resource_kind == "tl.so.tilo.letters.Desk"
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
        assert!(text.contains("so.tilo.letters"), "{text}");
        assert!(text.contains("action so.tilo.letters.send"), "{text}");
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
        let desk = host.root_of("so.tilo.letters").unwrap();
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
        let list = host.root_of("so.tilo.todo").unwrap().id_string();
        let notebook = host.root_of("so.tilo.notes").unwrap().id_string();
        let output = room
            .agent_invokes(
                HOST,
                "run-flow",
                &serde_json::json!({
                    "flow": "so.tilo.board.morning_cleanup",
                    "args": serde_json::json!({"list": list, "nb": notebook}).to_string(),
                }),
            )
            .unwrap()
            .value
            .unwrap();
        assert_eq!(output, serde_json::json!({"state": "done", "result": 1}));
    }
}
