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
    COMMIT, Committer, HOST_RESOURCE, Planner, host_resource, publish_package, publish_record,
};
use tilolang_tilo::runtime::{
    ActionInfo, BindTarget, Decision, EditOutcome, InvokeOutcome, Principal, Resolution,
    Suggestion, UiNode, Value, ViewEvent, ViewInstance,
};
use ui::LocalState;
use ui::tilo_actions::{ActionAddress, ControlInvocation, Resource};
use ui::tokens::{Rhythm, TextRole};
use ui::widget::arrange::{
    Arranged, Fit, FlexLayout, Grid, Length, Page as PageLayout, Row, Slot, Space, Stack,
    WidthClass, rem_of,
};
use ui::widget::controls::ControlResult;
use ui::widget::{
    AppKind, AppModel, AppWidget, Button, ButtonRole, Element, IconDescriptor, ModelPath, Spacer,
    Text, Widget,
};

use crate::i18n::t;
use crate::packages::EMBEDDED;
use crate::reconcile::{EventSink, Reconciler, UiEvent};

/// Where the window is.
enum Mode {
    Home,
    Open {
        app: String,
        view: Box<ViewInstance>,
        /// The views `open(r)` left, the most recent last: back returns to them.
        trail: Vec<ViewInstance>,
    },
}

/// A navigation the person asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Nav {
    Open(String),
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

/// The window. See the module docs.
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
    /// The packages under development (`tilolang run`), loaded in place of the built-in ones,
    /// and the version of them the host runs.
    dev: Option<Rc<RefCell<crate::dev::DevPackages>>>,
    dev_generation: u64,
}

fn slot(element: Element, fit: Fit) -> Slot {
    let mut slot = Slot::new(element);
    slot.fit = fit;
    slot
}

impl TilolangPage {
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
        let root = Stack::stack()
            .gap(Rhythm::Tight)
            .push_slot(slot(Box::new(Spacer::fixed(0.0)), Fit::hug()))
            .push_slot(slot(Box::new(Spacer::fixed(0.0)), Fit::fill(1.0)))
            .push_slot(slot(Box::new(Spacer::fixed(0.0)), Fit::hug()));
        Self {
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
            dev: None,
            dev_generation: 0,
        }
    }

    /// The window of `tilolang run`: it runs the packages under development instead of the
    /// built-in ones, opens the last of them, and follows each new build of their sources.
    #[must_use]
    pub fn developing(me: String, dev: Rc<RefCell<crate::dev::DevPackages>>) -> Self {
        let mut page = Self::new(me);
        page.dev = Some(dev);
        page
    }

    /// The host over the room record, built (with every embedded package) the first time the
    /// record is at hand.
    fn host_for(&self, doc: &ui::automerge::AutoCommit) -> Host {
        if let Some(host) = self.host.borrow().as_ref() {
            host.sync_from(doc);
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
        if self.dev.is_none() {
            for package in EMBEDDED {
                if let Err(error) = host.load_builtin(package.source, package.ids) {
                    tracing::error!(package = package.dir, %error, "a built-in package did not load");
                }
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

    /// Run the newest build of the packages under development: load it into the host (the room
    /// keeps its state) and open the open app again, or the package being developed the first
    /// time. A build with errors keeps the last good one running and says so.
    fn follow_dev(&mut self, host: &Host) {
        let Some(dev) = self.dev.clone() else {
            return;
        };
        let (generation, error, compiled, main) = {
            let dev = dev.borrow();
            (
                dev.generation(),
                dev.error().is_some(),
                dev.compiled().to_vec(),
                dev.main_app(),
            )
        };
        let shows_error = self.notice.as_deref() == Some(t("tilolang-dev-errors").as_str());
        if error != shows_error {
            self.notice = error.then(|| t("tilolang-dev-errors"));
            self.rebuild_chrome();
        }
        if generation == self.dev_generation {
            return;
        }
        let first = self.dev_generation == 0;
        self.dev_generation = generation;
        for package in &compiled {
            if let Err(error) = host.load_compiled(package) {
                tracing::error!(app = package.meta.app, %error, "a package under development did not load");
            }
        }
        let open = match &self.mode {
            Mode::Open { app, .. } => Some(app.clone()),
            Mode::Home if first => main,
            Mode::Home => None,
        };
        match open {
            // The views of the old version still run its code: open the app again.
            Some(app) => self.open(&app),
            None => self.show_home(),
        }
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

    /// What the window tells the person under the view, when anything.
    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
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
            Mode::Open { .. } => {
                let nav = Rc::clone(&self.inbox.nav);
                let back = Button::new("tilolang-back", &t("tilolang-back"))
                    .role(ButtonRole::Plain)
                    .on_view(move || nav.borrow_mut().push(Nav::Back));
                Box::new(
                    Row::row()
                        .fill_width()
                        .push_slot(slot(Box::new(back), Fit::hug()))
                        .with_id("tilolang-header"),
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
                self.mode = Mode::Open {
                    app: app.to_owned(),
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
        let tree = view.tree().clone();
        self.class = None;
        self.last_size = None;
        let body = self.reconciler.build(&tree);
        self.body_tree = Some(tree);
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
                let new_tree = view.tree().clone();
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
        self.body_tree = Some(new_tree.clone());
        self.refresh_bound();
    }

    /// Show what the bound fields and view state hold now in the controls that edit them.
    fn refresh_bound(&self) {
        let (Mode::Open { view, .. }, Some(tree)) = (&self.mode, self.body_tree.as_ref()) else {
            return;
        };
        visit_bound(self, view, tree);
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
        // Answered, one way or the other: the queued action is the door's now, or dropped.
        self.door_answered(result.as_ref().err().map(ToString::to_string));
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
        self.follow_dev(&host);
        if host.sync_from(doc) {
            self.refresh_bound();
        }
        if self.poll_view() {
            self.structure_dirty = true;
        }
        self.root.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let changed = self.handle_events();
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

        fn button(&self) -> String {
            first_button(self.page.body_tree.as_ref().expect("a view is open")).expect("a button")
        }
    }

    fn first_text(node: &UiNode) -> Option<String> {
        // The counter shows its value as a heading.
        if node.kind == "text" || node.kind == "heading" {
            return node.props.get("value").map(crate::reconcile::display);
        }
        node.children.iter().find_map(first_text)
    }

    /// The counter's `+1` button, else the first button.
    fn first_button(node: &UiNode) -> Option<String> {
        fn labelled(node: &UiNode, label: Option<&str>) -> Option<String> {
            let matches =
                label.is_none_or(|label| node.props.get("label") == Some(&Value::text(label)));
            if node.kind == "button" && matches {
                return Some(node.key.clone());
            }
            node.children
                .iter()
                .find_map(|child| labelled(child, label))
        }
        labelled(node, Some("+1")).or_else(|| labelled(node, None))
    }

    fn button_labelled(node: &UiNode, label: &str) -> Option<String> {
        if node.kind == "button" && node.props.get("label") == Some(&Value::text(label)) {
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
        room.open("so.tilo.notes");
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
        let key = room.button();
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
    fn the_notes_package_opens_and_a_new_note_is_created_through_the_door() {
        let mut room = Room::new();
        room.open("so.tilo.notes");
        let host = room.page.current_host().expect("the host started");
        // The notes are items of the notebook's collection, not resources of their own.
        let notes = |host: &Host| {
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
        assert_eq!(notes(&host), 0, "no notes yet");
        let key = room.button();
        room.page.events.borrow_mut().push(UiEvent::Press(key));
        room.frame();
        room.door();
        room.frame();
        assert_eq!(notes(&host), 1, "the press inserted a note");
    }
}
