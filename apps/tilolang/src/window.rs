//! The window of a tilolang app: its views rendered onto the native shell.
//!
//! The window runs the package's root view, and the view of the resource `open(r)` (or a row of
//! a list) went to. They are consecutive windows of one package in the lane, and they render as
//! the columns of one [`NavShell`]:
//!
//! - the root view's frame decides the shell: a `list` is the overview column when its items
//!   open into windows of their own (the next column shows the open one), else the content; a
//!   `document`, `page` or `canvas` is the content; its `sidebar {}` and `inspector(r) {}` are
//!   the shell's sidebar and inspector;
//! - the header of each column comes from the platform: the resource's title, the list's count,
//!   the actions on the resource (the primary one as the Primary button) and search;
//! - an enum window state a list sorts or filters by becomes the sort menu in the header and the
//!   filter list with counts in the sidebar.
//!
//! On a narrow screen the columns become push navigation with Back, by the shell's rules.
//!
//! Each frame the window
//!
//! 1. brings the runtime's mirror of the room record up to date with the record,
//! 2. re-runs a view if what it read changed and brings the shell up to date with its tree,
//! 3. takes what the person did and runs it in the runtime, which plans the change, and
//! 4. hands what the runtime planned to the door, as an invocation of the host's `commit`
//!    action, so the change is attributed, undoable and replayed by every peer.

mod describe;
mod list;
pub(crate) mod pane;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use serde_json::Value as Json;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{COMMIT, HOST_RESOURCE};
use tilolang_tilo::runtime::{
    ActionInfo, BindTarget, Decision, EditOutcome, InvokeOutcome, OutsideSink, Principal,
    Resolution, ResourceRef, Suggestion, UiNode, Value, ViewEvent, ViewInstance,
};
use tilolang_tilo::sink::LogSink;
use tilolang_tilo::{HostEffect, OpenEffect};
use ui::framework::{ResourceSurface, RunMode};
use ui::icons::Icon;
use ui::tilo_actions::{ActionAddress, ControlInvocation, InvocationId};
use ui::tokens::{Rhythm, TextRole};
use ui::widget::arrange::{Fit, Row, Slot, Stack};
use ui::widget::controls::ControlResult;
use ui::widget::toast::ToastKind;
use ui::widget::{
    Absent, AppModel, Button, ButtonRole, ContentKind, Element, EmptyState, MenuItem, ModelPath,
    NavItem, NavList, NavPane, NavSection, NavShell, Scrollable, Spacer, Text, Widget,
};

pub use list::{RowAction, RowData};

use crate::i18n::{t, t_args};
use crate::names::Names;
use crate::reconcile::{EventSink, Reconciler, UiEvent};
use crate::resource::{Live, Room, RoomState, caller_of};
use list::{Empty, ListPane};
use pane::Pane;

/// What the shell's controls ask of the window.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    /// Fire `event` of the node `key` of the root view (a sort or filter case).
    Fire { key: String, event: String },
    /// The row `key` of the list was selected: show its item in the next column.
    Select(String),
    /// The row `key` was activated: show its item and bring it to the front.
    Activate(String),
    /// Run a published action.
    Invoke {
        action: RowAction,
        /// What the action was run on, for the undo notice.
        title: String,
    },
    /// Back from the window shown in place of the root's content.
    Back,
}

pub(crate) type Requests = Rc<RefCell<Vec<Request>>>;

/// The participants at each resource of the package (by its id), from presence.
type Peers = HashMap<String, String>;

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

/// Hand everything the runtime committed under a ticket of `first` or later to the door.
fn queue_commits(host: &Host, outgoing: &Outgoing, first: u64, origin: Option<InvocationId>) {
    for committed in host.take_committed_from(first) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("ticket".to_owned(), Json::from(committed.ticket));
        if let Some(invocation) = ControlInvocation::from_parts(HOST_RESOURCE, COMMIT, arguments) {
            outgoing.borrow_mut().push_back((invocation, origin));
        }
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

// ----- frames ------------------------------------------------------------------------------

/// What a view's frame is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frame {
    List,
    Document,
    Page,
    Canvas,
}

impl Frame {
    fn of(node: &UiNode) -> Option<Self> {
        Some(match node.kind.as_str() {
            "list" => Self::List,
            "document" => Self::Document,
            "page" => Self::Page,
            "canvas" => Self::Canvas,
            _ => return None,
        })
    }

    fn content_kind(self) -> ContentKind {
        match self {
            Self::List => ContentKind::List,
            Self::Document => ContentKind::Document,
            Self::Page | Self::Canvas => ContentKind::Page,
        }
    }
}

/// The icon the app `app` declares, which its empty states show as native apps show theirs.
fn app_icon(host: &Host, app: &str) -> Option<ui::icons::Icon> {
    host.package(app)?
        .meta
        .icon
        .as_deref()
        .and_then(list::icon_named)
}

/// The frame of a view's tree: its root, or the first frame under the containers that hold it.
fn frame_of(tree: &UiNode) -> Option<&UiNode> {
    if Frame::of(tree).is_some() {
        return Some(tree);
    }
    tree.children.iter().find_map(frame_of)
}

fn part<'a>(frame: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
    frame.children.iter().find(|child| child.kind == kind)
}

/// A frame's content as the page the content column shows: everything in it but its sidebar and
/// inspector, at the readable measure (or the full width, for a canvas).
fn content_page(frame: &UiNode) -> UiNode {
    let bare = |kind: &str, key: String| UiNode {
        kind: kind.to_owned(),
        key,
        props: std::collections::BTreeMap::new(),
        binding: None,
        events: std::collections::BTreeMap::new(),
        anchor: None,
        children: Vec::new(),
    };
    // The frame's parts stand a section apart, as the sections of a native page do.
    let mut body = bare("stack", format!("{}~body", frame.key));
    body.props
        .insert("gap".to_owned(), Value::Text("section".to_owned()));
    let source = if frame.kind == "canvas" {
        without_frame_buttons(frame)
    } else {
        frame.clone()
    };
    body.children = source
        .children
        .into_iter()
        .filter(|child| !matches!(child.kind.as_str(), "sidebar" | "inspector" | "composer"))
        .collect();
    let mut page = bare("page", frame.key.clone());
    if frame.kind == "canvas" {
        page.props.insert("full".to_owned(), Value::Int(1));
    }
    page.children = vec![body];
    page
}

/// A sidebar's or inspector's content, as a stack.
fn side_stack(node: &UiNode) -> UiNode {
    UiNode {
        kind: "stack".to_owned(),
        ..node.clone()
    }
}

fn scrolling(body: Element) -> Element {
    Box::new(ui::widget::Scrollable::new_vertical().with_child(body))
}

fn unscroll(mut scroll: Element) -> Element {
    if let Some(scrollable) = scroll
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Scrollable>())
        && let Some(content) = scrollable.child_mut()
    {
        return std::mem::replace(content, Box::new(Spacer::fixed(0.0)));
    }
    scroll
}

/// A frame's composer as the page under its content: the same readable measure, so the two
/// share one content edge.
fn composer_page(frame: &UiNode) -> Option<UiNode> {
    let composer = part(frame, "composer")?;
    Some(UiNode {
        kind: "page".to_owned(),
        key: format!("{}~composer", frame.key),
        props: std::collections::BTreeMap::new(),
        binding: None,
        events: std::collections::BTreeMap::new(),
        anchor: None,
        children: vec![side_stack(composer)],
    })
}

/// A transcript: the content scrolls above the composer, which stays at the bottom of the
/// column; it opens at its newest line and follows it, as a conversation does.
fn transcript(content: Element, composer: Element) -> Element {
    Box::new(
        Stack::stack()
            .fill_width()
            .fill_height()
            .push_slot(slot(
                Box::new(
                    Scrollable::new_vertical()
                        .following_end()
                        .with_child(content),
                ),
                Fit::fill(1.0),
            ))
            .push_slot(slot(composer, Fit::hug())),
    )
}

/// The content and the composer of a [`transcript`], taken out to be reconciled.
fn untranscript(mut element: Element) -> Result<(Element, Element), Element> {
    let Some(stack) = element
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Stack>())
    else {
        return Err(element);
    };
    let mut slots = stack.take_slots().into_iter().map(Slot::into_element);
    match (slots.next(), slots.next()) {
        (Some(scroll), Some(composer)) => Ok((unscroll(scroll), composer)),
        _ => Err(element),
    }
}

/// Bring the pane up to date with `node` and the frame's composer `composer`: a frame with a
/// composer is a transcript ([`transcript`]); one without scrolls as a page.
fn fill_framed(
    pane: &mut Pane,
    reconciler: &mut Reconciler,
    built: (&mut Option<UiNode>, &mut Option<UiNode>),
    node: &UiNode,
    composer: Option<&UiNode>,
) {
    let (built_content, built_composer) = built;
    let Some(composer) = composer else {
        if built_composer.take().is_some() {
            *built_content = None;
        }
        fill(pane, reconciler, built_content, node);
        return;
    };
    let same = |old: &Option<UiNode>, new: &UiNode| {
        old.as_ref()
            .is_some_and(|old| old.key == new.key && old.kind == new.kind)
    };
    let kept = match (built_content.as_ref(), built_composer.as_ref()) {
        (Some(old_content), Some(old_composer))
            if same(built_content, node) && same(built_composer, composer) =>
        {
            untranscript(pane.take()).ok().map(|(content, footer)| {
                let content = if old_content == node {
                    content
                } else {
                    reconciler.update(content, old_content, node)
                };
                let footer = if old_composer == composer {
                    footer
                } else {
                    reconciler.update(footer, old_composer, composer)
                };
                transcript(content, footer)
            })
        }
        _ => None,
    };
    pane.set(
        kept.unwrap_or_else(|| transcript(reconciler.build(node), reconciler.build(composer))),
    );
    *built_content = Some(node.clone());
    *built_composer = Some(composer.clone());
}

/// Bring the pane up to date with `node`: reconcile what it shows when it was built from the
/// same node, else build it.
fn fill(pane: &mut Pane, reconciler: &mut Reconciler, built: &mut Option<UiNode>, node: &UiNode) {
    match built.as_ref() {
        Some(old) if old.key == node.key && old.kind == node.kind => {
            if old != node {
                let inner = unscroll(pane.take());
                let updated = reconciler.update(inner, old, node);
                pane.set(scrolling(updated));
            }
        }
        _ => pane.set(scrolling(reconciler.build(node))),
    }
    *built = Some(node.clone());
}

/// A choice an enum window state gives a list: its cases with their labels, the one chosen, and
/// for a filter how many items each case holds.
#[derive(Clone, Debug, PartialEq)]
struct Choice {
    cases: Vec<crate::reconcile::Case>,
    selected: Option<String>,
    counts: HashMap<String, i64>,
}

/// What a `list` frame shows.
struct Listing {
    key: String,
    rows: Vec<RowData>,
    empty: Empty,
    sort: Option<Choice>,
    filter: Option<Choice>,
    /// Whether the items open into windows of their own: their type has a default view.
    opens_items: bool,
    /// What an item is called ("note").
    noun: String,
}

/// The parts of the shell, which decide whether it is built again.
#[derive(Clone, Debug, PartialEq)]
struct Shape {
    frame: Frame,
    overview: bool,
    filter_sidebar: bool,
    view_sidebar: bool,
    content: ContentKind,
}

/// One window of the lane: a resource shown through a view.
struct Lane {
    target: ResourceRef,
    view: ViewInstance,
    tree: UiNode,
    events: EventSink,
    reconciler: Reconciler,
    /// What was built into the content, sidebar and inspector panes from it.
    content: Option<UiNode>,
    sidebar: Option<UiNode>,
    /// The composer built under the content, when the frame has one.
    composer: Option<UiNode>,
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
    /// Who else is at which resource of the package, as presence last reported it.
    peers: Peers,
    peers_changed: bool,
    root: Option<Lane>,
    /// The window `open(r)` or a row went to: the next column, or the content in place of the
    /// root's when the root is not a list.
    detail: Option<Lane>,
    requests: Requests,
    shape: Option<Shape>,
    /// The headers last given to the shell, so unchanged ones keep their controls.
    headers: HashMap<&'static str, String>,
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
    /// The destructive actions handed to the door and not yet answered, by action id, with the
    /// notice their undo shows.
    destructive: HashMap<String, String>,
    /// The primary actions handed to the door and not yet answered, by action id: what each
    /// makes opens when it is answered.
    making: HashSet<String>,
    undo: ui::undo::UndoAccess,
    /// The scenario a capture stages, until the host runs it.
    staged: Option<String>,
    started: bool,
    moved: bool,
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

/// Mark each input of a document's page with the role of the field it edits (`title`, `body`),
/// so the document draws its title as its large title and its body as running text, both in
/// place, as a native document does. Whether the page edits its title.
fn mark_field_roles(host: &Host, node: &mut UiNode) -> bool {
    let mut titled = false;
    if node.kind == "input"
        && let Some((target, field)) = node.binding.as_ref().and_then(|binding| binding.field())
        && let Some(role) = describe::type_of(host, &target.ty).and_then(|(_, ty)| {
            ty.fields
                .iter()
                .find(|declared| declared.id == *field)
                .and_then(|declared| declared.role.clone())
        })
    {
        titled = role == "title";
        node.props
            .insert("field_role".to_owned(), Value::Text(role));
    }
    for child in &mut node.children {
        titled |= mark_field_roles(host, child);
    }
    titled
}

/// Name each card of `node` after the resource it shows, and count what it lists when the view
/// gave no count: the head a dashboard card has natively (a title with a count pill).
fn mark_cards(host: &Host, node: &mut UiNode) {
    if node.kind == "card" {
        if !node.props.contains_key("title")
            && let Some(Value::Ref(reference)) = node.props.get("item")
        {
            let title = describe::title_of(host, &reference.target);
            node.props.insert("title".to_owned(), Value::Text(title));
        }
        if !node.props.contains_key("count") {
            let rows = rows_in(node);
            if rows > 0 {
                node.props.insert(
                    "count".to_owned(),
                    Value::Int(i64::try_from(rows).unwrap_or(i64::MAX)),
                );
            }
        }
    }
    if node.kind == "list_row"
        && !node.props.contains_key("title")
        && let Some(Value::Ref(reference)) = node.props.get("resource")
    {
        let target = reference.target.clone();
        node.props.insert(
            "title".to_owned(),
            Value::Text(describe::title_of(host, &target)),
        );
        if !node.props.contains_key("icon")
            && let Some((meta, _)) = describe::type_of(host, &target.ty)
            && let Some(icon) = meta.icon
        {
            node.props.insert("icon".to_owned(), Value::Text(icon));
        }
    }
    for child in &mut node.children {
        mark_cards(host, child);
    }
}

/// How many list rows `node` holds, under any containers.
fn rows_in(node: &UiNode) -> usize {
    node.children
        .iter()
        .map(|child| {
            if child.kind == "list_row" {
                1
            } else {
                rows_in(child)
            }
        })
        .sum()
}

/// The labels of the action buttons `node` draws in its content.
fn content_buttons<'a>(node: &'a UiNode, out: &mut Vec<&'a str>) {
    if node.kind == "button"
        && node.events.contains_key("press")
        && let Some(Value::Text(label)) = node.props.get("label")
    {
        out.push(label);
    }
    for child in &node.children {
        content_buttons(child, out);
    }
}

/// The buttons a canvas holds outside its cards: its own actions and flows, which a dashboard
/// offers in its header rather than as buttons floating over its cards.
fn frame_buttons(frame: &UiNode) -> Vec<UiNode> {
    fn walk(node: &UiNode, out: &mut Vec<UiNode>) {
        for child in &node.children {
            match child.kind.as_str() {
                "button" => out.push(child.clone()),
                "fragment" => walk(child, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    if frame.kind == "canvas" {
        walk(frame, &mut out);
    }
    out
}

/// `node` without the buttons [`frame_buttons`] lifted into the header.
fn without_frame_buttons(node: &UiNode) -> UiNode {
    let mut node = node.clone();
    node.children = node
        .children
        .into_iter()
        .filter(|child| child.kind != "button")
        .map(|child| {
            if child.kind == "fragment" {
                without_frame_buttons(&child)
            } else {
                child
            }
        })
        .collect();
    node
}

/// The actions of `actions` a header offers beside `frame`: those the content does not already
/// show as its own buttons, so no action stands twice in one window.
fn outside(actions: &[RowAction], frame: &UiNode) -> Rc<[RowAction]> {
    let mut shown = Vec::new();
    content_buttons(frame, &mut shown);
    actions
        .iter()
        .filter(|action| !shown.contains(&action.label.as_str()))
        .cloned()
        .collect::<Vec<_>>()
        .into()
}

fn slot(element: Element, fit: Fit) -> Slot {
    let mut slot = Slot::new(element);
    slot.fit = fit;
    slot
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
            .push_slot(slot(Box::new(NavShell::new()), Fit::fill(1.0)))
            .push_slot(slot(Box::new(Absent::new()), Fit::hug()))
            .push_slot(slot(endpoint, Fit::hug()))
            .push_slot(slot(open_endpoint, Fit::hug()));
        Self {
            names: Names::new(&me),
            app,
            me,
            mode,
            host,
            live: None,
            names_changed: Cell::new(false),
            peers: Peers::new(),
            peers_changed: false,
            root: None,
            detail: None,
            requests: Rc::default(),
            shape: None,
            headers: HashMap::new(),
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
            destructive: HashMap::new(),
            making: HashSet::new(),
            undo: ui::undo::UndoAccess::new(),
            staged: None,
            started: false,
            moved: false,
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

    /// This window sending the outside effects it settles to `sink` rather than the development
    /// log.
    #[must_use]
    pub fn with_sink(self, sink: impl OutsideSink + 'static) -> Self {
        *self.sink.borrow_mut() = Some(Box::new(sink));
        self
    }

    // ----- the host ------------------------------------------------------------------------

    /// The host over the room record, built with the package the first time the record is at
    /// hand.
    fn host_for(&mut self, doc: &ui::automerge::AutoCommit) -> Option<Host> {
        if let Some(host) = self.host.borrow().as_ref() {
            if host.sync_from(doc) {
                self.moved = true;
            }
            return Some(host.clone());
        }
        let built = Host::on_device(
            doc,
            Principal::new(self.me.clone()),
            self.device.as_deref(),
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
        host.runtime().set_name_host(self.names.clone());
        if let Some(sink) = self.sink.borrow_mut().take() {
            host.runtime().set_sink(BoxedSink(sink));
        }
        crate::app::load(&host);
        if self.mode != RunMode::Past {
            self.live = Some(Live::register(&host));
        }
        *self.host.borrow_mut() = Some(host.clone());
        Some(host)
    }

    fn current_host(&self) -> Option<Host> {
        self.host.borrow().clone()
    }

    /// The name a bound control is announced by: the field or view state it edits.
    fn field_names(&self) -> crate::reconcile::FieldNames {
        let shared = Rc::clone(&self.host);
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

    fn case_lookup(&self) -> crate::reconcile::Cases {
        let shared = Rc::clone(&self.host);
        Rc::new(move |id: &str| {
            shared
                .borrow()
                .as_ref()
                .map(|host| describe::cases(host, id))
                .unwrap_or_default()
        })
    }

    fn people(&self) -> crate::reconcile::People {
        let names = self.names.clone();
        Rc::new(move |id: &str| names.label(id))
    }

    fn lane(&self, target: ResourceRef, view: ViewInstance) -> Lane {
        let events: EventSink = Rc::default();
        Lane {
            target,
            tree: crate::reconcile::normalize(view.tree()),
            view,
            reconciler: Reconciler::new(Rc::clone(&events), self.field_names())
                .with_lookups(self.case_lookup(), self.people()),
            events,
            content: None,
            sidebar: None,
            composer: None,
        }
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
                self.root = Some(self.lane(target, view));
                self.detail = None;
                self.commit_planned();
            }
            Err(error) => {
                tracing::warn!(app = self.app, %error, "the app did not open");
                self.notice(&t("tilolang-open-failed"));
            }
        }
    }

    /// Show `target` in the window after the root's: what `open(r)` asked for, or a row.
    fn open_detail(&mut self, target: &ResourceRef, with: Option<&str>, front: bool) {
        let Some(host) = self.current_host() else {
            return;
        };
        if self
            .detail
            .as_ref()
            .is_some_and(|detail| detail.target == *target && with.is_none())
        {
            if front {
                self.show_content();
            }
            return;
        }
        match host.open_resource(target, with) {
            Ok(view) => {
                self.detail = Some(self.lane(target.clone(), view));
                self.compose();
                self.commit_planned();
                if front {
                    self.show_content();
                }
            }
            Err(error) => {
                tracing::warn!(%error, "a resource did not open");
                self.notice(&t("tilolang-open-failed"));
            }
        }
    }

    fn show_content(&mut self) {
        if let Some(shell) = self.shell() {
            shell.show(NavPane::Content);
        }
        self.structure_dirty = true;
    }

    fn close_detail(&mut self) {
        if self.detail.take().is_some() {
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
        if let Some(shown) = shown {
            self.open_detail(&shown, None, true);
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
            Ok(InvokeOutcome::Done(Value::Ref(shown))) => return Some(shown.target),
            Ok(InvokeOutcome::Done(_)) => {}
            Ok(other) => tracing::warn!(scenario = name, ?other, "the scenario did not run"),
            Err(error) => tracing::warn!(scenario = name, %error, "the scenario failed"),
        }
        None
    }

    // ----- the shell -----------------------------------------------------------------------

    fn shell(&mut self) -> Option<&mut NavShell> {
        self.body
            .children_mut()
            .into_iter()
            .next()?
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

    /// What the root's `list` frame shows.
    /// The row of the list item `item`, when it names what it shows.
    fn row_of(host: &Host, lane: &Lane, item: &UiNode, peers: &Peers) -> Option<RowData> {
        let Some(Value::Ref(reference)) = item.props.get("item") else {
            return None;
        };
        let target = reference.target.clone();
        let row = find_kind(item, "list_row").cloned();
        let checked = row
            .as_ref()
            .filter(|row| row.binding.is_some())
            .and_then(|row| match lane.view.bound_value(&row.key) {
                Some(Value::Bool(flag)) => Some(flag),
                Some(Value::Int(number)) => Some(number != 0),
                _ => None,
            });
        let avatar = row.as_ref().and_then(|row| match row.props.get("avatar") {
            Some(Value::Principal(who)) => Some((
                crate::reconcile::display(&Value::Principal(who.clone())),
                who.0.clone(),
            )),
            _ => None,
        });
        Some(RowData {
            key: item.key.clone(),
            label: describe::title_of(host, &target),
            actions: describe::offered_in(host, &target, &lane.target),
            presence: peers.get(&target.id_string()).cloned(),
            item: target,
            row,
            checked,
            avatar,
        })
    }

    /// The list `frame`'s choice `name` (`sort`, `filter`): its enum's cases, the one chosen
    /// and how many items each holds.
    fn choice(host: &Host, frame: &UiNode, name: &str) -> Option<Choice> {
        let Some(Value::Text(id)) = frame.props.get(&format!("{name}.enum")) else {
            return None;
        };
        let cases = describe::cases(host, id);
        let selected = frame
            .props
            .get(&format!("{name}.selected"))
            .and_then(describe::case_of);
        let counts = cases
            .iter()
            .filter_map(
                |case| match frame.props.get(&format!("{name}.count.{}", case.name)) {
                    Some(Value::Int(count)) => Some((case.name.clone(), *count)),
                    _ => None,
                },
            )
            .collect();
        Some(Choice {
            cases,
            selected,
            counts,
        })
    }

    fn listing(host: &Host, lane: &Lane, frame: &UiNode, peers: &Peers) -> Listing {
        let rows: Vec<RowData> = frame
            .children
            .iter()
            .filter(|child| child.kind == "item")
            .filter_map(|item| Self::row_of(host, lane, item, peers))
            .collect();
        let item_type = rows.first().map(|row| row.item.ty.clone());
        let declared_item = match frame.props.get("item_type") {
            Some(Value::Text(ty)) => Some(ty.clone()),
            _ => item_type,
        };
        let described = declared_item
            .as_deref()
            .and_then(|ty| describe::type_of(host, ty));
        let opens_items = described
            .as_ref()
            .is_some_and(|(meta, ty)| meta.default_view(&ty.id).is_some());
        let noun = described
            .as_ref()
            .map(|(_, ty)| describe::noun(ty))
            .unwrap_or_default();
        let filter = Self::choice(host, frame, "filter");
        let sort = Self::choice(host, frame, "sort");
        // An empty list offers the frame's primary action when nothing was made yet; one a filter
        // emptied says only that.
        let filtered = filter.as_ref().is_some_and(|filter| {
            filter.selected.is_some()
                && filter.selected.as_ref() != filter.cases.first().map(|case| &case.name)
        });
        let action = if filtered {
            None
        } else {
            describe::primary(host, &lane.target).or_else(|| {
                host.package(&lane.view_app(host))
                    .and_then(|package| describe::package_primary(&package.meta))
            })
        };
        let sentence = frame
            .props
            .get("empty")
            .map(crate::reconcile::display)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| t("tilolang-empty"));
        Listing {
            key: frame.key.clone(),
            rows,
            empty: Empty {
                sentence,
                icon: app_icon(host, &lane.view_app(host)),
                action,
            },
            sort,
            filter,
            opens_items,
            noun,
        }
    }

    /// Header buttons for `actions`, with the header's own controls (`controls`: sort, search)
    /// placed as native headers place them: the controls first, then the actions with an icon as
    /// icon buttons, a More menu of those without one, and the primary action last as the
    /// labelled Primary button, next to History.
    fn action_buttons(
        &self,
        actions: &[RowAction],
        title: &str,
        controls: Vec<Element>,
    ) -> Vec<Element> {
        let button = |index: usize, action: &RowAction| -> Element {
            let requests = Rc::clone(&self.requests);
            let request = Request::Invoke {
                action: action.clone(),
                title: title.to_owned(),
            };
            let id = format!("tilolang.action.{}.{}", action.action, index);
            let button = match action.icon {
                Some(icon) if action.primary => Button::new(&id, &action.label)
                    .with_icon(icon)
                    .role(ButtonRole::Primary),
                Some(icon) => {
                    Button::icon_only(&id, icon, &action.label).toggled(action.on.unwrap_or(false))
                }
                None if action.primary => Button::new(&id, &action.label).role(ButtonRole::Primary),
                None => Button::new(&id, &action.label).role(ButtonRole::Secondary),
            };
            Box::new(button.on_view(move || requests.borrow_mut().push(request.clone())))
        };
        let (primary, others): (Vec<_>, Vec<_>) = actions
            .iter()
            .enumerate()
            .partition(|(_, action)| action.primary);
        let (iconic, labelled): (Vec<_>, Vec<_>) = others
            .into_iter()
            .partition(|(_, action)| action.icon.is_some());
        let mut buttons: Vec<Element> = controls;
        buttons.extend(
            iconic
                .into_iter()
                .map(|(index, action)| button(index, action)),
        );
        if !labelled.is_empty() {
            let items = labelled
                .into_iter()
                .map(|(_, action)| {
                    let requests = Rc::clone(&self.requests);
                    let request = Request::Invoke {
                        action: action.clone(),
                        title: title.to_owned(),
                    };
                    let run = move || requests.borrow_mut().push(request.clone());
                    if action.destructive {
                        MenuItem::destructive_action(action.label.clone(), run)
                    } else {
                        MenuItem::action(action.label.clone(), run)
                    }
                })
                .collect();
            buttons.push(Box::new(
                Button::icon_only("tilolang.more", Icon::MoreHorizontal, &t("tilolang-more"))
                    .with_menu(items),
            ));
        }
        buttons.extend(
            primary
                .into_iter()
                .map(|(index, action)| button(index, action)),
        );
        buttons
    }

    /// A button of the root's canvas, in the header: pressing it presses the button of the view.
    fn lifted_button(&self, node: &UiNode) -> Element {
        let label = node
            .props
            .get("label")
            .map(crate::reconcile::display)
            .unwrap_or_default();
        let icon = match node.props.get("icon") {
            Some(Value::Text(name)) => list::icon_named(name),
            _ => None,
        };
        let primary =
            matches!(node.props.get("role"), Some(Value::Text(role)) if role == "primary");
        let id = format!("tilolang.frame.{}", node.key);
        let button = match icon {
            Some(icon) if primary => Button::new(&id, &label)
                .with_icon(icon)
                .role(ButtonRole::Primary),
            Some(icon) => Button::icon_only(&id, icon, &label),
            None if primary => Button::new(&id, &label).role(ButtonRole::Primary),
            None => Button::new(&id, &label).role(ButtonRole::Secondary),
        };
        let enabled = node.events.contains_key("press");
        let events = self
            .root
            .as_ref()
            .map(|lane| Rc::clone(&lane.events))
            .unwrap_or_default();
        let key = node.key.clone();
        Box::new(
            button
                .with_enabled(enabled)
                .on_view(move || events.borrow_mut().push(UiEvent::Press(key.clone()))),
        )
    }

    /// The header's sort menu for `sort` of the list `key`.
    fn sort_menu(&self, key: &str, sort: &Choice) -> Element {
        let items = sort
            .cases
            .iter()
            .map(|case| {
                let requests = Rc::clone(&self.requests);
                let request = Request::Fire {
                    key: key.to_owned(),
                    event: format!("sort.{}", case.name),
                };
                MenuItem::action(case.label.clone(), move || {
                    requests.borrow_mut().push(request.clone());
                })
                .with_checked(sort.selected.as_deref() == Some(case.name.as_str()))
            })
            .collect();
        Box::new(
            Button::icon_only("tilolang.sort", Icon::ArrowUpDown, &t("tilolang-sort"))
                .with_menu(items),
        )
    }

    fn filter_list(&self, key: &str, filter: &Choice) -> NavList {
        let items = filter
            .cases
            .iter()
            .map(|case| {
                let mut item = NavItem::new(case.name.clone(), case.label.clone());
                if let Some(icon) = case.icon {
                    item = item.icon(icon);
                }
                if let Some(count) = filter.counts.get(&case.name) {
                    item = item.count(u32::try_from(*count).unwrap_or(0));
                }
                item
            })
            .collect();
        let requests = Rc::clone(&self.requests);
        let key = key.to_owned();
        let mut list = NavList::new(vec![NavSection::untitled("tilolang.filter", items)])
            .with_id("tilolang.filter")
            .on_select(move |case| {
                requests.borrow_mut().push(Request::Fire {
                    key: key.clone(),
                    event: format!("filter.{case}"),
                });
            });
        if let Some(selected) = &filter.selected {
            list = list.selected(selected);
        }
        list
    }

    /// Give `column` a header, unless it already has this one.
    fn header(
        &mut self,
        column: &'static str,
        title: &str,
        signature: &str,
        actions: impl FnOnce(&Self) -> Vec<Element>,
    ) {
        let signature = format!("{title}\u{1f}{signature}");
        if self.headers.get(column) == Some(&signature) {
            return;
        }
        let actions = actions(self);
        let Some(shell) = self.shell() else {
            return;
        };
        match column {
            "sidebar" => shell.set_sidebar_header(title, actions),
            "overview" => shell.set_overview_header(title, actions),
            _ => shell.set_content_header(title, actions),
        }
        self.headers.insert(column, signature);
    }

    /// Bring the shell up to date with the windows of the lane.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the shell's columns, in order"
    )]
    fn compose(&mut self) {
        let Some(host) = self.current_host() else {
            return;
        };
        let Some(root) = self.root.as_ref() else {
            return;
        };
        let Some(frame) = frame_of(&root.tree).cloned() else {
            return;
        };
        let Some(kind) = Frame::of(&frame) else {
            return;
        };
        let listing =
            (kind == Frame::List).then(|| Self::listing(&host, root, &frame, &self.peers));
        let root_target = root.target.clone();
        let detail_frame = self
            .detail
            .as_ref()
            .and_then(|detail| frame_of(&detail.tree).cloned());
        let overview = listing.as_ref().is_some_and(|listing| listing.opens_items);
        let view_sidebar = part(&frame, "sidebar").is_some();
        let shape = Shape {
            frame: kind,
            overview,
            filter_sidebar: listing
                .as_ref()
                .is_some_and(|listing| listing.filter.is_some()),
            view_sidebar,
            content: if overview || (kind != Frame::List && self.detail.is_some()) {
                detail_frame
                    .as_ref()
                    .and_then(Frame::of)
                    .map_or(ContentKind::Document, Frame::content_kind)
            } else {
                kind.content_kind()
            },
        };
        if self.shape.as_ref() != Some(&shape) {
            self.build_shell(&shape);
            self.shape = Some(shape.clone());
        }
        let root_title = describe::title_of(&host, &root_target);
        let root_actions = describe::offered(&host, &root_target);
        let search: fn() -> Element = || {
            Box::new(ui::search::search_button(
                ui::search::SearchRequest::this_app(),
            ))
        };

        // The sidebar.
        if let Some(filter) = listing.as_ref().and_then(|listing| listing.filter.clone()) {
            let key = listing
                .as_ref()
                .map(|listing| listing.key.clone())
                .unwrap_or_default();
            let signature = format!("{filter:?}");
            if self.headers.get("filter") != Some(&signature) {
                let list = self.filter_list(&key, &filter);
                if let Some(shell) = self.shell() {
                    shell.set_sidebar(Some(Box::new(list)));
                }
                self.headers.insert("filter", signature);
            }
            self.header("sidebar", &root_title, "", |_| Vec::new());
        } else if let Some(sidebar) = part(&frame, "sidebar") {
            let node = side_stack(sidebar);
            let Self { body, root, .. } = self;
            if let (Some(shell), Some(lane)) = (shell_of(body), root.as_mut())
                && let Some(pane) = shell.sidebar_mut::<Pane>()
            {
                fill(pane, &mut lane.reconciler, &mut lane.sidebar, &node);
            }
            self.header("sidebar", &root_title, "", |_| Vec::new());
        }

        // The list, or the root's content.
        if let Some(listing) = &listing {
            let column = if overview { "overview" } else { "content" };
            let title = listing
                .filter
                .as_ref()
                .and_then(|filter| {
                    let selected = filter.selected.as_ref()?;
                    filter
                        .cases
                        .iter()
                        .find(|case| case.name == *selected)
                        .map(|case| case.label.clone())
                })
                .unwrap_or_else(|| root_title.clone());
            let signature = format!(
                "{:?}{:?}",
                root_actions
                    .iter()
                    .map(|action| &action.action)
                    .collect::<Vec<_>>(),
                listing.sort
            );
            let sort = listing.sort.clone();
            let key = listing.key.clone();
            let actions = Rc::clone(&root_actions);
            self.header(column, &title, &signature, |window| {
                let mut controls = Vec::new();
                if let Some(sort) = &sort {
                    controls.push(window.sort_menu(&key, sort));
                }
                controls.push(search());
                window.action_buttons(&actions, &title, controls)
            });
            let count = Some(listing.rows.len());
            let selected = self.detail.as_ref().and_then(|detail| {
                listing
                    .rows
                    .iter()
                    .find(|row| row.item == detail.target)
                    .map(|row| row.key.clone())
            });
            let rows = listing.rows.clone();
            let empty = listing.empty.clone();
            if let Some(shell) = self.shell() {
                if overview {
                    shell.set_overview_count(count);
                } else {
                    shell.set_content_count(count);
                }
                let pane = if overview {
                    shell.overview_mut::<ListPane>()
                } else {
                    shell.content_mut::<ListPane>()
                };
                if let Some(pane) = pane {
                    pane.show(rows, empty);
                    pane.select(selected);
                }
            }
        } else if self.detail.is_none() {
            let mut page = content_page(&frame);
            let titled = frame.kind == "document" && mark_field_roles(&host, &mut page);
            mark_cards(&host, &mut page);
            let root_actions = outside(&root_actions, &frame);
            let signature = format!(
                "{:?}",
                root_actions
                    .iter()
                    .map(|action| &action.action)
                    .collect::<Vec<_>>()
            );
            let actions = Rc::clone(&root_actions);
            let title = root_title.clone();
            let shown = if titled { "" } else { root_title.as_str() };
            let lifted = frame_buttons(&frame);
            let signature = format!(
                "{signature}{:?}",
                lifted
                    .iter()
                    .map(|button| (&button.key, &button.props))
                    .collect::<Vec<_>>()
            );
            self.header("content", shown, &signature, |window| {
                let mut controls = vec![search()];
                controls.extend(lifted.iter().map(|button| window.lifted_button(button)));
                window.action_buttons(&actions, &title, controls)
            });
            let composer = composer_page(&frame);
            let Self { body, root, .. } = self;
            if let (Some(shell), Some(lane)) = (shell_of(body), root.as_mut())
                && let Some(pane) = shell.content_mut::<Pane>()
            {
                fill_framed(
                    pane,
                    &mut lane.reconciler,
                    (&mut lane.content, &mut lane.composer),
                    &page,
                    composer.as_ref(),
                );
            }
        }

        // The window after the root's, in the content column.
        if overview || kind != Frame::List {
            if let (Some(detail), Some(detail_frame)) =
                (self.detail.as_ref(), detail_frame.as_ref())
            {
                let target = detail.target.clone();
                let mut page = content_page(detail_frame);
                // A document that edits its title shows it as its own large title, which the
                // header does not repeat (as in Notes).
                let titled = detail_frame.kind == "document" && mark_field_roles(&host, &mut page);
                mark_cards(&host, &mut page);
                let title = if titled {
                    String::new()
                } else {
                    describe::title_of(&host, &target)
                };
                let actions = outside(
                    &describe::offered_in(&host, &target, &root_target),
                    detail_frame,
                );
                let signature = format!(
                    "{:?}",
                    actions
                        .iter()
                        .map(|action| &action.action)
                        .collect::<Vec<_>>()
                );
                self.header("content", &title, &signature, |window| {
                    let controls = if overview { Vec::new() } else { vec![search()] };
                    window.action_buttons(&actions, &title, controls)
                });
                let composer = composer_page(detail_frame);
                let Self { body, detail, .. } = self;
                if let (Some(shell), Some(lane)) = (shell_of(body), detail.as_mut()) {
                    if shell.content_mut::<Pane>().is_none() {
                        shell.set_content(Box::new(Pane::new(Box::new(Spacer::fixed(0.0)))));
                        lane.content = None;
                        lane.composer = None;
                    }
                    if let Some(pane) = shell.content_mut::<Pane>() {
                        fill_framed(
                            pane,
                            &mut lane.reconciler,
                            (&mut lane.content, &mut lane.composer),
                            &page,
                            composer.as_ref(),
                        );
                    }
                    shell.set_can_go_back(kind != Frame::List);
                }
            } else if overview {
                let noun = listing
                    .as_ref()
                    .map(|listing| listing.noun.clone())
                    .unwrap_or_default();
                let sentence = t_args("tilolang-none-selected", &[("kind", noun.into())]);
                let signature = format!("empty\u{1f}{sentence}");
                if self.headers.get("content-empty") != Some(&signature) {
                    let placeholder = EmptyState::new(sentence);
                    let placeholder = match app_icon(&host, &self.app) {
                        Some(icon) => placeholder.with_icon(icon),
                        None => placeholder,
                    };
                    if let Some(shell) = self.shell() {
                        shell.set_content(Box::new(placeholder));
                        shell.set_content_header(String::new(), Vec::new());
                    }
                    self.headers.remove("content");
                    self.headers.insert("content-empty", signature);
                }
                return;
            }
            self.headers.remove("content-empty");
        }
    }

    /// Build the shell for `shape`, with empty columns the next pass fills.
    fn build_shell(&mut self, shape: &Shape) {
        let mut shell = NavShell::new().content_kind(shape.content);
        if shape.filter_sidebar {
            shell = shell.sidebar(Box::new(Spacer::fixed(0.0)));
        } else if shape.view_sidebar {
            shell = shell.sidebar(Box::new(Pane::new(Box::new(Spacer::fixed(0.0)))));
        }
        let list = || {
            Box::new(ListPane::new(
                &self.requests,
                &self
                    .root
                    .as_ref()
                    .map(|lane| Rc::clone(&lane.events))
                    .unwrap_or_default(),
                String::new(),
            )) as Element
        };
        if shape.frame == Frame::List {
            if shape.overview {
                shell = shell
                    .overview(list())
                    .content(Box::new(EmptyState::new(String::new())));
            } else {
                shell = shell.content(list());
            }
        } else {
            shell = shell.content(Box::new(Pane::new(Box::new(Spacer::fixed(0.0)))));
            let requests = Rc::clone(&self.requests);
            shell = shell.on_back(|| false, move || requests.borrow_mut().push(Request::Back));
        }
        self.headers.clear();
        if let Some(lane) = self.root.as_mut() {
            lane.content = None;
            lane.sidebar = None;
        }
        if let Some(lane) = self.detail.as_mut() {
            lane.content = None;
        }
        self.set_body_slot(0, Box::new(shell), Fit::fill(1.0));
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
                        .map(describe::label_of)
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

    /// Re-run the views whose reads changed; whether any tree changed.
    fn poll_views(&mut self) -> bool {
        let mut changed = false;
        for lane in [self.root.as_mut(), self.detail.as_mut()]
            .into_iter()
            .flatten()
        {
            match lane.view.poll_patches() {
                Ok(patches) if patches.is_empty() => {}
                Ok(_) => {
                    lane.tree = crate::reconcile::normalize(lane.view.tree());
                    changed = true;
                }
                Err(error) => tracing::warn!(%error, "a view failed to run"),
            }
        }
        changed
    }

    /// The windows the views asked to open.
    fn follow_opens(&mut self) {
        let mut opens = Vec::new();
        for lane in [self.root.as_mut(), self.detail.as_mut()]
            .into_iter()
            .flatten()
        {
            opens.extend(lane.view.poll_events());
        }
        for event in opens {
            match event {
                ViewEvent::Open { target, view } => {
                    self.open_detail(&target, view.as_deref(), true);
                }
            }
        }
    }

    /// Show what the bound fields and view state hold now in the controls that edit them.
    fn refresh_bound(&self) {
        for lane in [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
        {
            visit_bound(lane, &lane.tree);
        }
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
                        self.open_detail(&target, None, true);
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
        if !std::mem::take(&mut self.settle_due) || self.mode != RunMode::Live {
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

    fn commit_planned(&mut self) {
        // A past version shows what it held; nothing it does is committed.
        if self.mode == RunMode::Past {
            return;
        }
        let Some(host) = self.current_host() else {
            return;
        };
        queue_commits(&host, &self.outgoing, 0, None);
        self.settle_due = true;
    }

    /// The door answered the oldest commit handed over.
    fn door_answered(&mut self, error: Option<String>) {
        let Some(ticket) = self.in_flight.pop_front() else {
            return;
        };
        if let Some(host) = self.current_host() {
            let _ = host.store().take_ticket(ticket);
        }
        if let Some(error) = error {
            self.notice(&error);
        }
    }

    // ----- what the person did -------------------------------------------------------------

    fn handle_requests(&mut self) -> bool {
        let requests = std::mem::take(&mut *self.requests.borrow_mut());
        let changed = !requests.is_empty();
        for request in requests {
            let front = matches!(request, Request::Activate(_));
            match request {
                Request::Fire { key, event } => {
                    if let Some(lane) = &self.root
                        && let Err(error) = lane.view.fire(&key, &event)
                    {
                        tracing::warn!(%error, key, event, "a choice did not apply");
                    }
                }
                Request::Select(key) | Request::Activate(key) => {
                    let target = self.root.as_ref().and_then(|lane| {
                        frame_of(&lane.tree)?
                            .children
                            .iter()
                            .find(|item| item.key == key)
                            .and_then(|item| match item.props.get("item") {
                                Some(Value::Ref(reference)) => Some(reference.target.clone()),
                                _ => None,
                            })
                    });
                    if let Some(target) = target {
                        let opens = self.shape.as_ref().is_some_and(|shape| shape.overview);
                        if opens {
                            self.open_detail(&target, None, front);
                        }
                    }
                }
                Request::Invoke { action, title } => {
                    if action.destructive {
                        let notice = if title.is_empty() {
                            action.label.clone()
                        } else {
                            t_args(
                                "tilolang-did",
                                &[
                                    ("action", action.label.as_str().into()),
                                    ("title", title.as_str().into()),
                                ],
                            )
                        };
                        self.destructive.insert(action.action.clone(), notice);
                    }
                    if action.primary {
                        self.making.insert(action.action.clone());
                    }
                    let arguments = action
                        .args
                        .iter()
                        .map(|(name, value)| (name.clone(), Json::from(value.clone())))
                        .collect();
                    if let Some(invocation) =
                        ControlInvocation::from_parts(&action.resource, &action.action, arguments)
                    {
                        self.outgoing.borrow_mut().push_back((invocation, None));
                    }
                }
                Request::Back => self.close_detail(),
            }
        }
        changed
    }

    fn handle_events(&mut self) -> bool {
        let mut changed = self.handle_requests();
        let answers = std::mem::take(&mut *self.answers.borrow_mut());
        for answer in answers {
            changed = true;
            self.answer(answer);
        }
        let mut events = Vec::new();
        for (index, lane) in [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .enumerate()
        {
            if let Some(lane) = lane {
                events.extend(
                    std::mem::take(&mut *lane.events.borrow_mut())
                        .into_iter()
                        .map(|event| (index, event)),
                );
            }
        }
        for (index, event) in events {
            changed = true;
            self.event(index, &event);
        }
        changed
    }

    fn event(&mut self, lane: usize, event: &UiEvent) {
        let Some(view) = (if lane == 0 {
            self.root.as_ref()
        } else {
            self.detail.as_ref()
        })
        .map(|lane| &lane.view) else {
            return;
        };
        let outcome = match event {
            UiEvent::Press(key) => match view.press(key) {
                Ok(InvokeOutcome::Done(_)) => Ok(()),
                Ok(InvokeOutcome::Suggested(suggestion)) => {
                    self.ask = Some(suggestion);
                    self.rebuild_ask();
                    Ok(())
                }
                Ok(InvokeOutcome::Denied) => Err(t("tilolang-ask-denied")),
                Err(error) => Err(error.to_string()),
            },
            UiEvent::Edit(key, text) => edit(view, key, Value::Text(text.clone())),
            UiEvent::Toggle(key, checked) => edit(view, key, Value::Bool(*checked)),
            UiEvent::Resolve(key, approve) => self.resolve(view, key, *approve),
            UiEvent::Fire(key, event) => match view.fire(key, event) {
                Ok(_) => Ok(()),
                Err(error) => Err(error.to_string()),
            },
            UiEvent::Open(target) => {
                let target = target.clone();
                self.open_detail(&target, None, true);
                Ok(())
            }
        };
        if let Err(message) = outcome {
            self.notice(&message);
            self.refresh_bound();
        }
        self.commit_planned();
        self.follow_opens();
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
    /// Whether it was the window's own (a commit, or an action its shell ran).
    fn answered(&mut self, address: &ActionAddress, error: Option<String>) -> bool {
        if address.resource == HOST_RESOURCE && address.action == COMMIT {
            self.door_answered(error);
            return true;
        }
        if let Some(notice) = self.destructive.remove(&address.action) {
            match error {
                None => {
                    if let Some(shell) = self.shell() {
                        shell.toast_with_action(notice, ToastKind::Info, t("tilolang-undo"));
                    }
                }
                Some(error) => self.notice(&error),
            }
            return true;
        }
        if self.making.remove(&address.action) && error.is_none() {
            // What a primary action made opens beside the list, as the person asked for it.
            if let Some(made) = self
                .current_host()
                .and_then(|host| host.take_produced(&address.action))
            {
                self.open_requests.borrow_mut().push(made.id_string());
            }
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

    /// The root view's tree.
    #[must_use]
    pub fn root_tree(&self) -> Option<&UiNode> {
        self.root.as_ref().map(|lane| &lane.tree)
    }

    /// The tree of the window after the root's.
    #[must_use]
    pub fn detail_tree(&self) -> Option<&UiNode> {
        self.detail.as_ref().map(|lane| &lane.tree)
    }

    /// The resource the window after the root's shows.
    #[must_use]
    pub fn detail_target(&self) -> Option<&ResourceRef> {
        self.detail.as_ref().map(|lane| &lane.target)
    }

    /// Report what a person did to the root view (`detail: false`) or the window after it.
    pub fn act(&self, detail: bool, event: UiEvent) {
        let lane = if detail { &self.detail } else { &self.root };
        if let Some(lane) = lane {
            lane.events.borrow_mut().push(event);
        }
    }

    /// Ask what the shell's controls ask: choose a case, select or activate a row, run an
    /// action.
    #[cfg(test)]
    pub(crate) fn request(&self, request: Request) {
        self.requests.borrow_mut().push(request);
    }

    /// The shell.
    #[must_use]
    pub fn shell_ref(&self) -> Option<&NavShell> {
        self.body
            .children()
            .into_iter()
            .next()?
            .as_any()?
            .downcast_ref::<NavShell>()
    }

    /// The host, once the window has the record.
    #[must_use]
    pub fn host(&self) -> Option<Host> {
        self.current_host()
    }
}

/// The first node of `kind` under `node`.
fn find_kind<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
    node.children.iter().find_map(|child| {
        (child.kind == kind)
            .then_some(child)
            .or_else(|| find_kind(child, kind))
    })
}

fn shell_of(body: &mut Element) -> Option<&mut NavShell> {
    body.children_mut()
        .into_iter()
        .next()?
        .as_any_mut()?
        .downcast_mut::<NavShell>()
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

fn visit_bound(lane: &Lane, node: &UiNode) {
    if node.binding.is_some()
        && let Some(value) = lane.view.bound_value(&node.key)
    {
        match value {
            Value::Bool(flag) => lane.reconciler.set_flag(&node.key, flag),
            Value::Int(0) if node.kind == "checkbox" || node.kind == "toggle" => {
                lane.reconciler.set_flag(&node.key, false);
            }
            Value::Int(1) if node.kind == "checkbox" || node.kind == "toggle" => {
                lane.reconciler.set_flag(&node.key, true);
            }
            other => lane
                .reconciler
                .set_text(&node.key, &crate::reconcile::display(&other)),
        }
    }
    for child in &node.children {
        visit_bound(lane, child);
    }
}

impl Lane {
    /// The app of the package whose view this is.
    fn view_app(&self, host: &Host) -> String {
        describe::type_of(host, &self.target.ty)
            .map(|(meta, _)| meta.app)
            .unwrap_or_default()
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
fn peers_of(presence: &ui::presence::PresenceCtx, app: &str, me: &str) -> Peers {
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
        let (doc, _) = ui::doc::document_ref(state, path);
        let Some(host) = self.host_for(doc) else {
            self.body.sync(state, path);
            return;
        };
        if !self.started {
            self.started = true;
            self.open_root(&host);
            self.compose();
        }
        if self.staged.is_some() {
            self.run_staged(&host);
        }
        if std::mem::take(&mut self.moved) {
            self.refresh_bound();
            self.settle_due = true;
        }
        if self.poll_views() {
            self.compose();
            self.refresh_bound();
            self.structure_dirty = true;
        }
        self.body.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let door = self.take_door_work();
        let opened = self.take_open_requests();
        let settled = self.settle_outbox();
        if self.names_changed.take() {
            for lane in [self.root.as_ref(), self.detail.as_ref()]
                .into_iter()
                .flatten()
            {
                lane.view.refresh();
            }
        }
        let peers = std::mem::take(&mut self.peers_changed);
        if peers {
            self.compose();
        }
        let changed = self.handle_events() || door || opened || settled || peers;
        if changed && self.poll_views() {
            self.compose();
            self.refresh_bound();
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
        if peers != self.peers {
            self.peers = peers;
            self.peers_changed = true;
        }
        self.body.draw(scene, transform, text, theme, presence);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.structure_dirty);
        self.body.take_structure_dirty() || dirty
    }
}
