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
mod transcript;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value as Json;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{COMMIT, HOST_RESOURCE};
use tilolang_tilo::runtime::{
    ActionInfo, BindTarget, Decision, EditOutcome, InvokeOutcome, OutsideSink, Patch, Principal,
    Resolution, ResourceRef, Suggestion, UiNode, Value, ViewEvent, ViewInstance,
};
use tilolang_tilo::sink::LogSink;
use tilolang_tilo::{HostEffect, OpenEffect};
use ui::framework::{ResourceSurface, RunMode};
use ui::icons::Icon;
use ui::tilo_actions::{ActionAddress, ControlInvocation, InvocationId};
use ui::tokens::{MotionSource, Rhythm, TextRole};
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
use list::{Empty, ListPane, Peers, RowSource};
use pane::Pane;
use transcript::{Thread, ThreadSource};

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

/// The frame of a view's tree as the window composes it: a page inside it drawn as a stack
/// ([`crate::reconcile::normalize`]), and a `list` without its rows, which the list column
/// reads from the view as it shows them.
fn composed_frame(tree: &UiNode) -> Option<UiNode> {
    let frame = frame_of(tree)?;
    if frame.kind == "list" {
        return Some(UiNode {
            children: Vec::new(),
            ..frame.clone()
        });
    }
    Some(crate::reconcile::normalize(frame))
}

/// The keys from the root of `tree` to its node `key`, as patches name a place.
fn key_path(tree: &UiNode, key: &str) -> Option<Vec<String>> {
    if tree.key == key {
        return Some(vec![tree.key.clone()]);
    }
    tree.children.iter().find_map(|child| {
        let mut path = key_path(child, key)?;
        path.insert(0, tree.key.clone());
        Some(path)
    })
}

/// Render every row of the lists of `view` but those of `skip` (the list column's and a
/// transcript's, which render the rows they show): a list inside a page is drawn whole, one in a card as its preview (the rows
/// the runtime built for it).
/// What moves a column the person's last input brings to the front: a click or a tap slides it
/// in; a key, an agent's action through the door, or nothing the person just did lands it.
fn by_input() -> MotionSource {
    ui::motion::recent_input(ui::time::now()).unwrap_or(MotionSource::Action)
}

fn show_content_rows(view: &mut ViewInstance, skip: &[Option<&str>]) {
    fn lists(node: &UiNode, skip: &[Option<&str>], out: &mut Vec<String>) {
        if node.kind == "list" {
            if skip.contains(&Some(node.key.as_str())) {
                return;
            }
            out.push(node.key.clone());
        }
        for child in &node.children {
            lists(child, skip, out);
        }
    }
    let mut shown = HashSet::new();
    loop {
        let mut found = Vec::new();
        lists(view.tree(), skip, &mut found);
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
    // A transcript spans its column, as a native conversation does, rather than a reading
    // measure.
    if frame.kind == "canvas" || part(frame, "composer").is_some() {
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

/// A frame's composer as the page under its content: the same readable measure, so the two
/// share one content edge.
fn composer_page(frame: &UiNode) -> Option<UiNode> {
    let mut composer = side_stack(part(frame, "composer")?);
    iconic_buttons(&mut composer);
    let mut props = std::collections::BTreeMap::new();
    props.insert("full".to_owned(), Value::Int(1));
    Some(UiNode {
        kind: "page".to_owned(),
        key: format!("{}~composer", frame.key),
        props,
        binding: None,
        events: std::collections::BTreeMap::new(),
        anchor: None,
        children: vec![composer],
    })
}

/// A composer's buttons that have an icon show only it, as a native composer's Send does.
fn iconic_buttons(node: &mut UiNode) {
    if node.kind == "button" && node.props.contains_key("icon") {
        node.props
            .insert("presentation".to_owned(), Value::Text("icon".to_owned()));
    }
    for child in &mut node.children {
        iconic_buttons(child);
    }
}

/// A transcript: the content scrolls above the composer, which stays at the bottom of the
/// column; it opens at its newest line and follows it, as a conversation does.
fn transcript(content: Element, composer: Element) -> Element {
    above_composer(
        Box::new(
            Scrollable::new_vertical()
                .following_end()
                .with_child(content),
        ),
        composer,
    )
}

/// `top`, which scrolls itself, filling the column above `composer`.
fn above_composer(top: Element, composer: Element) -> Element {
    Box::new(
        Stack::stack()
            .fill_width()
            .fill_height()
            .push_slot(slot(top, Fit::fill(1.0)))
            .push_slot(slot(composer, Fit::hug())),
    )
}

/// The transcript of a frame whose content is one list ([`transcript::thread_list`]): where its
/// rows come from, and the reconciler that builds them.
struct ThreadOf {
    source: Rc<ThreadSource>,
    reconciler: Reconciler,
}

/// The thread the transcript `element` shows, when it shows one.
fn thread_in(element: &mut Element) -> Option<&mut Thread> {
    element
        .as_any_mut()?
        .downcast_mut::<Stack>()?
        .child_mut(0)?
        .as_any_mut()?
        .downcast_mut::<Thread>()
}

/// Reconcile what the scroll view `scroll` shows, built for `old`, with `new` in place: the
/// content keeps its widgets and the view its scroll position. Whether `scroll` is a scroll
/// view.
fn reconcile_scrolled(
    reconciler: &mut Reconciler,
    scroll: &mut Element,
    old: &UiNode,
    new: &UiNode,
) -> bool {
    let Some(scroll) = scroll
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Scrollable>())
    else {
        return false;
    };
    let Some(content) = scroll.child_mut() else {
        return false;
    };
    if let Some(replaced) = reconciler.update_in(content, old, new) {
        scroll.replace_child(replaced);
    }
    true
}

/// Reconcile a [`transcript`] in place: its content, built for `old_content`, with
/// `content`, and its composer, built for `old_composer`, with `composer`. Whether `element`
/// is a transcript.
fn reconcile_transcript(
    reconciler: &mut Reconciler,
    element: &mut Element,
    (old_content, content): (&UiNode, &UiNode),
    (old_composer, composer): (&UiNode, &UiNode),
    thread: Option<&ThreadOf>,
) -> bool {
    let shown = thread_in(element)
        .map(|shown| thread.is_some_and(|thread| shown.model().reads(&thread.source)));
    let Some(stack) = element
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Stack>())
    else {
        return false;
    };
    if stack.len() != 2 {
        return false;
    }
    // A thread takes its rows' changes as they come (`apply_polled`): it stays while it shows
    // the same list. A scrolled content is reconciled in place.
    let kept = match (shown, thread) {
        (Some(same), _) => same,
        (None, Some(_)) => false,
        (None, None) => stack
            .child_mut(0)
            .is_some_and(|scroll| reconcile_scrolled(reconciler, scroll, old_content, content)),
    };
    if !kept {
        return false;
    }
    let replaced = stack
        .child_mut(1)
        .and_then(|footer| reconciler.update_in(footer, old_composer, composer));
    if let Some(replaced) = replaced {
        stack.replace(1, replaced);
    }
    true
}

/// Bring the pane up to date with `node` and the frame's composer `composer`: a frame with a
/// composer is a transcript ([`transcript`]); one without scrolls as a page.
fn fill_framed(
    pane: &mut Pane,
    reconciler: &mut Reconciler,
    built: (&mut Option<UiNode>, &mut Option<UiNode>),
    (node, composer): (&UiNode, Option<&UiNode>),
    thread: Option<ThreadOf>,
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
            reconcile_transcript(
                reconciler,
                pane.content_mut(),
                (old_content, node),
                (old_composer, composer),
                thread.as_ref(),
            )
        }
        _ => false,
    };
    if !kept {
        let footer = reconciler.build(composer);
        pane.set(match thread {
            Some(ThreadOf { source, reconciler }) => {
                above_composer(Box::new(transcript::thread(source, reconciler)), footer)
            }
            None => transcript(reconciler.build(node), footer),
        });
    }
    *built_content = Some(node.clone());
    *built_composer = Some(composer.clone());
}

/// Bring the pane up to date with `node`: reconcile what it shows in place when it was built
/// from the same node, else build it.
fn fill(pane: &mut Pane, reconciler: &mut Reconciler, built: &mut Option<UiNode>, node: &UiNode) {
    let kept = match built.as_ref() {
        Some(old) if old.key == node.key && old.kind == node.kind => {
            old == node || reconcile_scrolled(reconciler, pane.content_mut(), old, node)
        }
        _ => false,
    };
    if !kept {
        pane.set(scrolling(reconciler.build(node)));
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
    /// The view, shared with the list column, which renders the rows it shows.
    view: Rc<RefCell<ViewInstance>>,
    /// The `list` frame whose rows the list column shows: the root's, when its frame is one.
    list: Option<String>,
    /// The `list` a frame with a composer shows as its transcript, rendering the rows it shows.
    thread: Option<String>,
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

/// Whether `node` shows nothing: it holds containers only, no text, control or card.
fn shows_nothing(node: &UiNode) -> bool {
    node.children.iter().all(|child| {
        matches!(
            child.kind.as_str(),
            "stack" | "column" | "fragment" | "row" | "grid" | "page" | "layers" | "spacer"
        ) && shows_nothing(child)
    })
}

/// How many rows the lists under `node` hold: every row, shown or not (a card shows a preview).
fn rows_in(node: &UiNode) -> usize {
    node.children
        .iter()
        .map(|child| {
            if child.kind == "list" {
                child.items_total()
            } else if child.kind == "list_row" {
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
            peers: Peers::default(),
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
        // `now` is the toolkit's clock, as every time the window shows is: a capture pins it, so
        // a scenario's messages are stamped with the capture's time.
        host.set_clock(|| {
            ui::clock::local_now()
                .with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        });
        if let Some(sink) = self.sink.borrow_mut().take() {
            host.runtime().set_sink(BoxedSink(sink));
        }
        crate::app::load(&host);
        if self.mode != RunMode::Past {
            // What is published and found of the room is read off the frame the first time;
            // the window takes it in when it is ready.
            let wake = ui::wake::WakeHandle::new();
            self.live = Some(Live::register(&host, Arc::new(move || wake.wake())));
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

    /// The window of `target` through `view`; the root's (`root`) shows the rows of its `list`
    /// frame in the list column.
    fn lane(&self, target: ResourceRef, mut view: ViewInstance, root: bool) -> Lane {
        let events: EventSink = Rc::default();
        let list = frame_of(view.tree())
            .filter(|frame| root && frame.kind == "list")
            .map(|frame| frame.key.clone());
        let thread = frame_of(view.tree())
            .and_then(transcript::thread_list)
            .map(|list| list.key.clone());
        show_content_rows(&mut view, &[list.as_deref(), thread.as_deref()]);
        Lane {
            target,
            view: Rc::new(RefCell::new(view)),
            list,
            thread,
            reconciler: Reconciler::new(Rc::clone(&events), self.field_names())
                .with_lookups(self.case_lookup(), self.people()),
            events,
            content: None,
            sidebar: None,
            composer: None,
        }
    }

    /// The transcript of `lane`'s frame `frame`, when its content is one list beside a
    /// composer: the lane renders that list's rows as the transcript shows them.
    fn thread_of(&self, lane: &Lane, frame: &UiNode) -> Option<ThreadOf> {
        let list = transcript::thread_list(frame)?.key.clone();
        Some(ThreadOf {
            source: Rc::new(ThreadSource {
                view: Rc::clone(&lane.view),
                list,
            }),
            reconciler: Reconciler::new(Rc::clone(&lane.events), self.field_names())
                .with_lookups(self.case_lookup(), self.people()),
        })
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
                self.root = Some(self.lane(target, view, true));
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
                self.detail = Some(self.lane(target.clone(), view, false));
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

    fn shell(&mut self) -> Option<&mut NavShell> {
        self.body
            .children_mut()
            .into_iter()
            .next()?
            .as_any_mut()?
            .downcast_mut::<NavShell>()
    }

    /// The list column: the overview when the items open beside it, else the content.
    fn list_pane(&mut self) -> Option<&mut ListPane> {
        let overview = self.shape.as_ref().is_some_and(|shape| shape.overview);
        let shell = self.shell()?;
        if overview {
            shell.overview_mut::<ListPane>()
        } else {
            shell.content_mut::<ListPane>()
        }
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

    /// What the root's `list` frame shows, but its rows: the list column reads those.
    fn listing(host: &Host, lane: &Lane, frame: &UiNode) -> Listing {
        let declared_item = match frame.props.get("item_type") {
            Some(Value::Text(ty)) => Some(ty.clone()),
            _ => None,
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
        let Some(frame) = composed_frame(root.view.borrow().tree()) else {
            return;
        };
        let Some(kind) = Frame::of(&frame) else {
            return;
        };
        let listing = (kind == Frame::List).then(|| Self::listing(&host, root, &frame));
        let root_target = root.target.clone();
        let source = listing.as_ref().map(|listing| {
            Rc::new(RowSource {
                view: Rc::clone(&root.view),
                list: listing.key.clone(),
                host: host.clone(),
                owner: root_target.clone(),
                peers: Rc::clone(&self.peers),
                names: self.names.clone(),
            })
        });
        let detail_frame = self
            .detail
            .as_ref()
            .and_then(|detail| composed_frame(detail.view.borrow().tree()));
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
            let shown = self.detail.as_ref().map(|detail| detail.target.clone());
            let empty = listing.empty.clone();
            let count = self.list_pane().map(|pane| {
                pane.set_empty(empty);
                if let Some(source) = &source {
                    pane.read(source);
                }
                let selected = shown.as_ref().and_then(|target| pane.key_of(target));
                pane.select(selected);
                pane.len()
            });
            if let Some(shell) = self.shell() {
                if overview {
                    shell.set_overview_count(count);
                } else {
                    shell.set_content_count(count);
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
            let thread = self
                .root
                .as_ref()
                .and_then(|lane| self.thread_of(lane, &frame));
            if let Some(lane) = self.root.as_mut() {
                lane.thread = thread.as_ref().map(|thread| thread.source.list.clone());
            }
            // A canvas with nothing on it (a board pointing at nothing yet) says so, as a native
            // app's empty window does, rather than showing a blank page.
            let blank = frame.kind == "canvas" && composer.is_none() && shows_nothing(&page);
            let icon = app_icon(&host, &self.app);
            let Self { body, root, .. } = self;
            if let (Some(shell), Some(lane)) = (shell_of(body), root.as_mut()) {
                if blank {
                    if shell.content_mut::<EmptyState>().is_none() {
                        let empty = EmptyState::new(t("tilolang-empty"));
                        let empty = match icon {
                            Some(icon) => empty.with_icon(icon),
                            None => empty,
                        };
                        shell.set_content(Box::new(empty));
                        lane.content = None;
                        lane.composer = None;
                    }
                } else {
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
                            (&page, composer.as_ref()),
                            thread,
                        );
                    }
                }
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
                let thread = self
                    .detail
                    .as_ref()
                    .and_then(|lane| self.thread_of(lane, detail_frame));
                if let Some(lane) = self.detail.as_mut() {
                    lane.thread = thread.as_ref().map(|thread| thread.source.list.clone());
                }
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
                            (&page, composer.as_ref()),
                            thread,
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

    /// Re-run the views whose reads changed, and sort what changed by where it shows.
    fn poll_views(&self) -> Polled {
        let mut polled = Polled::default();
        for lane in [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
        {
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
            let list = lane
                .list
                .as_deref()
                .and_then(|list| key_path(view.tree(), list));
            let thread = lane
                .thread
                .as_deref()
                .and_then(|thread| key_path(view.tree(), thread));
            let content = polled.content;
            polled.content = false;
            let changes = polled.sort(patches, list.as_deref(), thread.as_deref());
            if polled.content {
                // A list inside the content may have rows the view has not rendered yet.
                show_content_rows(&mut view, &[lane.list.as_deref(), lane.thread.as_deref()]);
            }
            polled.content |= content;
            if let Some(thread) = &lane.thread
                && (changes.reread || !changes.is_empty())
            {
                polled
                    .threads
                    .push((Rc::clone(&lane.view), thread.clone(), changes));
            }
        }
        polled
    }

    /// Bring the window up to date with what the views changed: rows of the list column in
    /// place, the columns reconciled when anything else changed. Whether anything did.
    fn apply_polled(&mut self, polled: Polled) -> bool {
        let Polled {
            list:
                ListChanges {
                    structure,
                    rows,
                    reread,
                },
            threads,
            mut content,
        } = polled;
        let mut rows_changed = !structure.is_empty() || !rows.is_empty();
        for (view, list, changes) in threads {
            rows_changed |= !changes.is_empty();
            content |= self.apply_thread(view, list, changes);
        }
        if let Some(pane) = self.list_pane() {
            if reread {
                pane.forget();
            } else if rows_changed {
                pane.changed(&structure, rows);
            }
        }
        if content || reread || !structure.is_empty() {
            self.compose();
        }
        if content || reread {
            self.refresh_bound();
        }
        content || reread || rows_changed
    }

    /// Apply what the view `view` changed of its transcript's list `list` to the transcript
    /// the content column shows, when it shows that one. Whether the columns are to be composed
    /// again: the view replaced the list as a whole, so the transcript is built anew.
    fn apply_thread(
        &mut self,
        view: Rc<RefCell<ViewInstance>>,
        list: String,
        changes: ListChanges,
    ) -> bool {
        if changes.reread {
            for lane in [self.root.as_mut(), self.detail.as_mut()]
                .into_iter()
                .flatten()
            {
                if Rc::ptr_eq(&lane.view, &view) {
                    lane.content = None;
                }
            }
            return true;
        }
        let source = ThreadSource { view, list };
        let thread = self
            .shell()
            .and_then(|shell| shell.content_mut::<Pane>())
            .and_then(|pane| thread_in(pane.content_mut()))
            .filter(|thread| thread.model().reads(&source));
        if let Some(thread) = thread {
            list::apply_changes(thread, &changes.structure, changes.rows);
        }
        false
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
                ViewEvent::Open { target, view } => {
                    self.open_detail(&target, view.as_deref(), Some(by_input()));
                }
            }
        }
    }

    /// Show what the bound fields and view state hold now in the controls that edit them: the
    /// columns' and the rows the list column shows.
    fn refresh_bound(&mut self) {
        for lane in [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
        {
            visit_bound(lane, lane.view.borrow().tree());
        }
        if let Some(pane) = self.list_pane() {
            pane.refresh_shown();
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
            let front = matches!(request, Request::Activate(_)).then(by_input);
            match request {
                Request::Fire { key, event } => {
                    if let Some(lane) = &self.root {
                        let fired = lane.view.borrow().fire(&key, &event);
                        if let Err(error) = fired {
                            tracing::warn!(%error, key, event, "a choice did not apply");
                        }
                    }
                }
                Request::Select(key) | Request::Activate(key) => {
                    let target = self
                        .list_pane()
                        .and_then(|pane| pane.find(&key))
                        .map_or_else(
                            || {
                                let lane = self.root.as_ref()?;
                                let view = lane.view.borrow();
                                match find_kind(view.find(&key)?, "item")?.props.get("item") {
                                    Some(Value::Ref(reference)) => {
                                        Some(ResourceRef::clone(&reference.target))
                                    }
                                    _ => None,
                                }
                            },
                            |row| Some(row.item),
                        );
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
        .map(|lane| Rc::clone(&lane.view)) else {
            return;
        };
        let outcome = match event {
            UiEvent::Press(key) => match view.borrow().press(key) {
                Ok(InvokeOutcome::Done(_)) => Ok(()),
                Ok(InvokeOutcome::Suggested(suggestion)) => {
                    self.ask = Some(suggestion);
                    self.rebuild_ask();
                    Ok(())
                }
                Ok(InvokeOutcome::Denied) => Err(t("tilolang-ask-denied")),
                Err(error) => Err(error.to_string()),
            },
            UiEvent::Edit(key, text) => edit(&view.borrow(), key, Value::Text(text.clone())),
            UiEvent::Toggle(key, checked) => edit(&view.borrow(), key, Value::Bool(*checked)),
            UiEvent::Resolve(key, approve) => self.resolve(&view.borrow(), key, *approve),
            UiEvent::Fire(key, event) => match view.borrow().fire(key, event) {
                Ok(_) => Ok(()),
                Err(error) => Err(error.to_string()),
            },
            UiEvent::Open(target) => {
                let target = target.clone();
                self.open_detail(&target, None, Some(by_input()));
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
                        shell.toast_with_action(
                            notice,
                            ToastKind::Info,
                            "undo-2",
                            t("tilolang-undo"),
                        );
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

    /// The root view's tree, as the window draws it: the rows of its list are those the list
    /// shows.
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

/// The first node of `kind` at or under `node`.
fn find_kind<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
    if node.kind == kind {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_kind(child, kind))
}

/// What a view changed of a list whose rows the window renders as it shows them.
#[derive(Default)]
struct ListChanges {
    /// Rows inserted, removed or moved, in order.
    structure: Vec<Patch>,
    /// The rows whose content changed.
    rows: Vec<String>,
    /// The list was replaced as a whole: its rows are read anew.
    reread: bool,
}

impl ListChanges {
    fn is_empty(&self) -> bool {
        self.structure.is_empty() && self.rows.is_empty()
    }
}

/// Where a patch is, against a list whose rows the window renders.
enum Place {
    /// Among the list's own children: a row inserted, removed or moved.
    Rows,
    /// Inside the row of this key.
    Row(String),
    /// Above the list, changing what holds it.
    Above,
    Elsewhere,
}

impl Place {
    /// Where a patch at `path` (`structural` when it inserts, removes or moves) is, against the
    /// list at `list` (its keys from the root).
    fn of(path: &[String], structural: bool, list: &[String]) -> Self {
        if structural && path == list {
            Self::Rows
        } else if path.len() > list.len() && path.starts_with(list) {
            path.get(list.len())
                .map_or(Self::Elsewhere, |row| Self::Row(row.clone()))
        } else if structural && list.starts_with(path) {
            Self::Above
        } else {
            Self::Elsewhere
        }
    }

    /// Take `patch` into `changes` when it is among or inside the list's rows; the patch back
    /// when it is not.
    fn take(self, patch: Patch, changes: &mut ListChanges) -> Option<Patch> {
        match self {
            Self::Rows => changes.structure.push(patch),
            Self::Row(row) => {
                if !changes.rows.contains(&row) {
                    changes.rows.push(row);
                }
            }
            Self::Above | Self::Elsewhere => return Some(patch),
        }
        None
    }
}

/// What the views changed, by where it shows.
#[derive(Default)]
struct Polled {
    /// What the list column's list changed.
    list: ListChanges,
    /// What the transcripts' lists changed: the view and list of each.
    threads: Vec<(Rc<RefCell<ViewInstance>>, String, ListChanges)>,
    /// Something the columns show besides those rows changed.
    content: bool,
}

impl Polled {
    /// Sort `patches` of a view whose list column shows the list at `list`, and whose
    /// transcript the list at `thread` (their keys from the root), when it has them. Returns
    /// what changed of the transcript's list.
    fn sort(
        &mut self,
        patches: Vec<Patch>,
        list: Option<&[String]>,
        thread: Option<&[String]>,
    ) -> ListChanges {
        let mut threaded = ListChanges::default();
        for patch in patches {
            let (path, structural) = match &patch {
                Patch::SetRoot(_) => {
                    self.content = true;
                    self.list.reread |= list.is_some();
                    threaded.reread |= thread.is_some();
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
            let at_list = list.map_or(Place::Elsewhere, |list| Place::of(path, structural, list));
            let at_thread = thread.map_or(Place::Elsewhere, |thread| {
                Place::of(path, structural, thread)
            });
            let above = (
                matches!(at_list, Place::Above),
                matches!(at_thread, Place::Above),
            );
            let Some(untaken) = at_list.take(patch, &mut self.list) else {
                continue;
            };
            if at_thread.take(untaken, &mut threaded).is_none() {
                continue;
            }
            self.content = true;
            self.list.reread |= above.0;
            threaded.reread |= above.1;
        }
        threaded
    }
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
    // The rows of the list column are the list's to refresh.
    if lane.list.as_deref() == Some(node.key.as_str()) {
        return;
    }
    if node.binding.is_some()
        && let Some(value) = lane.view.borrow().bound_value(&node.key)
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
            if let Some(live) = &self.live {
                live.update();
            }
        } else if let Some(live) = self.live.as_ref().filter(|live| live.pending()) {
            live.update();
        }
        let polled = self.poll_views();
        self.apply_polled(polled);
        self.body.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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
        // Who is at an item, and what participants are called, show on the rows.
        let peers = std::mem::take(&mut self.peers_changed);
        if (peers || renamed)
            && let Some(pane) = self.list_pane()
        {
            pane.refresh_shown();
        }
        let changed = self.handle_events() || door || opened || settled || peers || renamed;
        if changed {
            let polled = self.poll_views();
            self.apply_polled(polled);
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
        if peers != *self.peers.borrow() {
            *self.peers.borrow_mut() = peers;
            self.peers_changed = true;
        }
        self.body.draw(scene, transform, text, theme, presence);
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
