//! Running views and blocks: the view frame, the UI tree built from it, and the resolution of
//! foreign view nodes (`view:<app>:<view id>`) into nested boundaries.

use std::collections::HashSet;
use std::rc::Rc;

use crate::colls::{CollSeen, merge_seen};
use crate::core::Core;
use crate::error::RuntimeError;
use crate::handles::{FrameKind, Retained, ViewState};
use crate::host_ui::VIEW_STATE;
use crate::ids::{ModuleHash, Principal, ViewId};
use crate::meta::ParamMeta;
use crate::registry::Package;
use crate::space::Space;
use crate::value::{RefValue, Value};
use crate::view::{LIST_LEN, NodeBuild, PREVIEW, UiNode};
use crate::view_state::StateKey;

/// Nested foreign views deeper than this are refused: a module that applies views in a loop
/// would otherwise exhaust the host's stack.
const MAX_VIEW_DEPTH: usize = 32;

/// The prefix of the kind of a node that applies another package's view.
const FOREIGN_VIEW: &str = "view:";
/// The kind of a node that renders a block's source (`embed(source, author: who)`).
pub(crate) const EMBED: &str = "embed";
/// The kind of a node that draws a resource through its type's default view (`show(r)`): a
/// call of that view, which package's it is known only once the resource is.
const SHOW: &str = "show";
/// The property of a `show` node holding the resource it draws.
const SHOWN: &str = "of";
/// The property of a `show` node naming the view it draws the resource through (its id or
/// name); empty or absent for its type's default view.
const SHOWN_AS: &str = "view";
/// `show(r, row: true)`: draw the resource as a list of its type draws each item, through the
/// row its type declares.
const SHOWN_AS_ROW: &str = "row";
/// The prefix of the properties of such a node that carry the view's arguments.
const ARG_PREFIX: &str = "arg.";
/// The prefix of the kind of the node a UI loop lowers to (`for:<app>:<row view id>`): one row
/// per item of its `items`, each the row view called with the item and the node's `arg.<name>`
/// properties, and a boundary of its own.
const FOR_EACH: &str = "for:";
/// The property of a loop's node that holds its items.
pub const ITEMS: &str = "items";
/// The property of a `ui.VirtualList` naming the item where the person looking has seen
/// nothing yet (`first_unseen`): the list draws its divider over the row that shows it.
pub const UNSEEN_FROM: &str = "unseen_from";
/// The kind of the divider over the first row the person looking has not seen.
/// The property of a loop's node that names the parameter of its row view the item goes to.
const EACH: &str = "each";
/// The property of a loop's node that holds the key suffix of the rows around it.
const SUFFIX: &str = "suffix";
/// The parameter of a row view that takes the row's key suffix (`<suffix>/<item key>`).
const ROW_KEY: &str = "$key";

/// A view export of a loaded module.
#[derive(Clone, Debug)]
pub struct ViewTarget {
    pub module: ModuleHash,
    pub export: String,
    /// The id of the view it renders, `""` for a block's view, which no type offers.
    pub view: String,
    pub params: Vec<ParamMeta>,
}

/// A rendered view: its tree, what it read, and the action references it retained.
pub struct Rendered {
    pub root: UiNode,
    pub reads: Vec<(String, String)>,
    /// What it saw of the collections it read item by item.
    pub(crate) colls: Vec<CollSeen>,
    pub space_reads: Vec<(String, f64)>,
    pub retained: Vec<Retained>,
    /// The view state slots read, with their generations.
    pub state_reads: Vec<(StateKey, u64)>,
    /// The value each slot read had, its default included.
    pub state_seen: Vec<(StateKey, Value)>,
    /// When the time it read next reads differently (seconds since 1970, UTC); `None` when it
    /// read no time.
    pub changes_at: Option<i64>,
}

pub(crate) struct Acc {
    pub reads: Vec<(String, String)>,
    pub colls: Vec<CollSeen>,
    pub space_reads: Vec<(String, f64)>,
    pub retained: Vec<Retained>,
    pub state_reads: Vec<(StateKey, u64)>,
    pub state_seen: Vec<(StateKey, Value)>,
    pub changes_at: Option<i64>,
}

/// The earlier of two instants at which something read changes; `None` stands for never.
pub(crate) fn earliest(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// What a view boundary is offered: its space, the window and the person looking at it, and
/// how deep the nesting is.
#[derive(Clone, Copy)]
pub(crate) struct Offer<'a> {
    pub space: Space,
    pub viewer: &'a Principal,
    pub window: &'a str,
    pub depth: usize,
}

impl Offer<'_> {
    pub fn deeper(self) -> Self {
        Self {
            depth: self.depth.saturating_add(1),
            ..self
        }
    }
}

pub(crate) struct Build<'a> {
    pub nodes: &'a [NodeBuild],
    pub offer: Offer<'a>,
}

/// Where the walk over a view's nodes is.
#[derive(Clone, Copy)]
pub(crate) struct Walk<'a> {
    pub level: usize,
    /// The key the nodes here go under, `None` at the root of a view.
    pub prefix: Option<&'a str>,
    /// Whether the nodes here are the rows of a `list`.
    pub in_list: bool,
    /// Whether the nodes here are a row of a `list` that stands a divider where its unseen part
    /// starts (`unseen_from:`), which already says of a row there that it is new.
    pub says_new: bool,
    /// How many rows that list shows, when it is a card's preview (its `preview` property):
    /// the rows after those are never shown, so they are not built.
    pub preview: Option<usize>,
}

impl<'a> Walk<'a> {
    /// The walk of `n`'s children, reached under `key` from `walk`.
    fn below(walk: Self, n: &'a NodeBuild, key: &'a str) -> Self {
        // A view call's keys are the same at every call site, so they go under the call's key.
        let calls = n.kind == "fragment" || n.kind.starts_with(FOREIGN_VIEW) || n.kind == SHOW;
        let listed = crate::view::is_virtual(&n.kind);
        Self {
            level: walk.level.saturating_add(1),
            prefix: if calls { Some(key) } else { walk.prefix },
            in_list: listed,
            says_new: walk.says_new,
            preview: match n.props.get(PREVIEW) {
                Some(Value::Int(rows)) if n.kind == "list" => usize::try_from(*rows).ok(),
                _ => None,
            },
        }
    }
}

/// A view call met while building a tree: the boundary the host renders.
pub(crate) struct Stub<'a> {
    /// The key the call's node has in the tree.
    pub key: &'a str,
    /// Shared: every row of a loop calls the one view.
    pub target: Rc<ViewTarget>,
    pub args: Vec<Value>,
    /// A row of a `list`: rendered when the host shows it, not now.
    pub lazy: bool,
    pub depth: usize,
}

/// Renders the view calls of a tree as boundaries of their own.
pub(crate) trait Boundaries {
    /// The children of the `fragment` that stands for the call: the callee's tree, or nothing
    /// for a row that is not shown yet, or for a boundary whose tree the caller keeps.
    fn boundary(&mut self, stub: Stub<'_>) -> Result<Vec<UiNode>, RuntimeError>;

    /// Take the rows of a loop over a collection to keep: their boundaries are met next.
    fn rows(&mut self, rows: LoopRows);

    /// Whether the rows of a `list` are virtual: built when the host shows them, and absent
    /// from the tree until then.
    fn virtualizes(&self) -> bool;

    /// The rows of a `list` that are virtual: the ones built so far that still stand, as the
    /// list's children, in order. Nothing is built per item: the list's length and each row's
    /// key are read from `rows` when the host asks.
    fn virtual_rows(&mut self, rows: VirtualRows) -> Result<Vec<UiNode>, RuntimeError>;
}

/// What the rows of a virtual list are made from: the items of a collection, read from its
/// listing as they are needed, or a list of values.
#[derive(Clone)]
pub(crate) enum RowSource {
    Coll(Rc<crate::value::CollRef>),
    List(Rc<[Value]>),
}

/// The rows of a `list` that the host shows as it scrolls: the list's key, how each row is
/// made, and from what.
pub(crate) struct VirtualRows {
    pub list: String,
    pub rows: RowsOf,
    pub source: RowSource,
}

/// How a boundary is rendered: under which key, and who renders its view calls.
pub(crate) struct Scoped<'p, 'b> {
    pub prefix: Option<&'p str>,
    /// Whether the tree rendered is a row of a `list` that stands a divider where its unseen
    /// part starts (`unseen_from:`) (see [`Walk::says_new`]).
    pub says_new: bool,
    pub boundaries: Option<&'b mut dyn Boundaries>,
}

/// Prefix every key of a nested tree with the key of the node that holds it. A view called
/// per row, or a block embedded in two messages, would otherwise repeat its keys, and a key
/// must name one node in the tree for a press or an edit to find it.
pub(crate) fn scope(prefix: &str, mut tree: UiNode) -> UiNode {
    // An anchored node keeps its key: the anchor is the address people and agents use.
    if tree.anchor.is_none() {
        tree.key = format!("{prefix}~{}", tree.key);
    }
    tree.children = tree
        .children
        .into_iter()
        .map(|c| scope(prefix, c))
        .collect();
    tree
}

impl Acc {
    /// Take on what a nested boundary read and retained, and hand back its tree.
    pub fn absorb(&mut self, nested: Rendered) -> UiNode {
        self.reads.extend(nested.reads);
        merge_seen(&mut self.colls, nested.colls);
        self.space_reads.extend(nested.space_reads);
        self.retained.extend(nested.retained);
        self.state_reads.extend(nested.state_reads);
        self.state_seen.extend(nested.state_seen);
        self.changes_at = earliest(self.changes_at, nested.changes_at);
        nested.root
    }
}

impl Core {
    /// The target of the view with this id.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown view.
    pub fn view_target(&self, view: &str) -> Result<ViewTarget, RuntimeError> {
        let pkg = self
            .registry
            .borrow()
            .view_package(view)
            .ok_or_else(|| RuntimeError::NotFound(format!("view {view}")))?;
        let meta = pkg
            .view(view)
            .ok_or_else(|| RuntimeError::NotFound(format!("view {view}")))?;
        Ok(ViewTarget {
            module: pkg.hash.clone(),
            export: format!("v_{view}"),
            view: view.to_owned(),
            params: meta.params.clone(),
        })
    }

    /// Call a view's own function for an item it answers for (`on show`), in a frame of the
    /// window the view runs in: the action it makes reads and sets that window's state, as one
    /// the view's render makes does.
    pub(crate) fn call_in_view(
        &self,
        pkg: &Package,
        export: &str,
        params: &[ParamMeta],
        args: &[Value],
        window: &str,
        viewer: &Principal,
        result: &str,
    ) -> Result<Value, RuntimeError> {
        self.frames.borrow_mut().push(
            pkg.hash.clone(),
            FrameKind::View(ViewState {
                space: Space::default(),
                window: window.to_owned(),
                viewer: viewer.clone(),
                ..ViewState::default()
            }),
        );
        let out = self.call_in_frame(pkg, export, params, args, Some(result));
        self.frames.borrow_mut().pop();
        out
    }

    /// Render a view as its own boundary. `author` replaces the module as the author of what
    /// the view creates: an embedded block is written by its principal, not by its module.
    /// View calls inside are resolved in place, their reads joining this view's.
    pub(crate) fn render_at(
        &self,
        target: &ViewTarget,
        args: &[Value],
        offer: Offer<'_>,
        author: Option<&ModuleHash>,
    ) -> Result<Rendered, RuntimeError> {
        self.render_boundary(target, args, offer, author, None)
    }

    /// Render a view as its own boundary, its keys under `scope` (the key of the node that
    /// holds it, `None` at the root). With `boundaries`, a view call inside is not resolved
    /// here but handed to it: the node becomes a `fragment` holding what it answers, and the
    /// callee's reads are its own, not this view's.
    pub(crate) fn render_boundary(
        &self,
        target: &ViewTarget,
        args: &[Value],
        offer: Offer<'_>,
        author: Option<&ModuleHash>,
        scoped: Option<Scoped<'_, '_>>,
    ) -> Result<Rendered, RuntimeError> {
        let (space, depth) = (offer.space, offer.depth);
        if depth > MAX_VIEW_DEPTH {
            return Err(RuntimeError::Package(format!(
                "views are nested more than {MAX_VIEW_DEPTH} deep"
            )));
        }
        // The window's own view: the one its bar switched the resource to, when its type
        // offers more than one. The state is read here and noted in the frame once it is
        // popped, so choosing another view in the bar runs the window again.
        let (chosen, chosen_state) = if depth == 0 {
            self.window_view(target, args, offer)
        } else {
            (None, None)
        };
        let target = chosen.as_ref().unwrap_or(target);
        let pkg = self
            .registry
            .borrow()
            .package(&target.module)
            .ok_or_else(|| RuntimeError::NotFound(format!("module {}", target.module)))?;
        // A written view's module is its writer's code wherever it renders: at its root, in the
        // rows its loops render, in whatever view of its own it calls.
        let author = author
            .cloned()
            .or_else(|| self.written_author(&pkg.hash))
            .unwrap_or_else(|| pkg.hash.clone());
        self.frames.borrow_mut().push(
            author,
            FrameKind::View(ViewState {
                space,
                window: offer.window.to_owned(),
                viewer: offer.viewer.clone(),
                ..ViewState::default()
            }),
        );
        let root = self.call_in_frame(&pkg, &target.export, &target.params, args, Some("View"));
        // The host's surfaces are read while the view's frame still stands: the actions they
        // show are the declarations of what it rendered, presented as the view presents them.
        let surfaced = match &root {
            Ok(Value::Node(root)) => self.surfaces(*root, args, &target.view),
            _ => Ok(()),
        };
        let frame = self.frames.borrow_mut().pop();
        let Some(FrameKind::View(state)) = frame.map(|f| f.kind) else {
            return Err(RuntimeError::Package("the view frame vanished".into()));
        };
        let Value::Node(root) = root? else {
            return Err(RuntimeError::Package("a view must return a ui node".into()));
        };
        surfaced?;
        let mut acc = Acc {
            reads: state.reads,
            colls: state.colls,
            space_reads: state.space_reads,
            retained: Vec::new(),
            state_reads: state.state_reads,
            state_seen: state.state_seen,
            changes_at: state.changes_at,
        };
        if let Some((key, generation, seen)) = chosen_state {
            acc.state_reads.push((key.clone(), generation));
            acc.state_seen.push((key, seen));
        }
        let cx = Build {
            nodes: &state.nodes,
            offer,
        };
        let (prefix, says_new, hook) = match scoped {
            Some(Scoped {
                prefix,
                says_new,
                boundaries,
            }) => (prefix, says_new, boundaries),
            None => (None, false, None),
        };
        let walk = Walk {
            level: 0,
            prefix,
            in_list: false,
            says_new,
            preview: None,
        };
        let tree = self.build_node(&cx, root, walk, &mut acc, hook)?;
        Ok(Rendered {
            root: tree,
            reads: acc.reads,
            colls: acc.colls,
            space_reads: acc.space_reads,
            retained: acc.retained,
            state_reads: acc.state_reads,
            state_seen: acc.state_seen,
            changes_at: acc.changes_at,
        })
    }

    /// Render a view as a boundary, and when it fails for want of a reading that has not
    /// arrived yet (its app publishes nothing yet, or the session has not published its
    /// record), the waiting node in its place: the whole tree stands, and this place shows that
    /// reading is still coming rather than failing the screen around it. The boundary reads the
    /// slot that moves when the reading comes, so it runs again then, and the reading is asked
    /// of the host in the meantime. A failure for any other reason is the caller's to report.
    pub(crate) fn render_boundary_waiting(
        &self,
        target: &ViewTarget,
        args: &[Value],
        offer: Offer<'_>,
        scoped: Option<Scoped<'_, '_>>,
    ) -> Result<Rendered, RuntimeError> {
        match self.render_boundary(target, args, offer, None, scoped) {
            Ok(rendered) => Ok(rendered),
            Err(error) if self.read_waited(&error) => Ok(self.waiting_rendered()),
            Err(error) => Err(error),
        }
    }

    /// Whether `error` is a read waiting on a reading that has not arrived: kept per message,
    /// so each of several places waiting on its own reading is told apart from a fault beside it.
    fn read_waited(&self, error: &RuntimeError) -> bool {
        self.foreign.borrow().waiting(&error.to_string())
    }

    /// What a view call waiting on a reading renders as it stands: the toolkit's waiting state
    /// (its skeleton, saying "Loading" to assistive technology), keyed by where the call
    /// stands, with the slot that moves when the reading comes noted as read so the call runs
    /// again then.
    fn waiting_rendered(&self) -> Rendered {
        let mut acc = Acc {
            reads: Vec::new(),
            colls: Vec::new(),
            space_reads: Vec::new(),
            retained: Vec::new(),
            state_reads: Vec::new(),
            state_seen: Vec::new(),
            changes_at: None,
        };
        // The call runs again when the reading it waited on comes, or when the package of the
        // app whose type no loaded package declared arrives ([`crate::foreign::packages_key`]).
        for key in [
            crate::foreign::arrived_key(),
            crate::foreign::packages_key(),
        ] {
            let generation = self.view_states.borrow().generation(&key);
            let seen = self
                .view_states
                .borrow()
                .get(&key)
                .cloned()
                .unwrap_or_else(Value::none);
            acc.state_reads.push((key.clone(), generation));
            acc.state_seen.push((key, seen));
        }
        let root = UiNode {
            kind: "ui.Skeleton".to_owned(),
            key: "waiting".to_owned(),
            props: std::collections::BTreeMap::from([("shape".to_owned(), Value::text("page"))]),
            binding: None,
            events: std::collections::BTreeMap::new(),
            anchor: None,
            children: Vec::new(),
        };
        Rendered {
            root,
            reads: acc.reads,
            colls: acc.colls,
            space_reads: acc.space_reads,
            retained: acc.retained,
            state_reads: acc.state_reads,
            state_seen: acc.state_seen,
            changes_at: acc.changes_at,
        }
    }

    fn build_node<'h>(
        &self,
        cx: &Build<'_>,
        at: usize,
        walk: Walk<'_>,
        acc: &mut Acc,
        mut hook: Option<&mut (dyn Boundaries + 'h)>,
    ) -> Result<UiNode, RuntimeError> {
        if walk.level > cx.nodes.len() {
            return Err(RuntimeError::Package("the ui tree contains a cycle".into()));
        }
        let n = cx
            .nodes
            .get(at)
            .ok_or_else(|| RuntimeError::Package("unknown ui node".into()))?;
        // An anchored node keeps its key: the anchor is the address people and agents use.
        let key = match (walk.prefix, &n.anchor) {
            (Some(prefix), None) => format!("{prefix}~{}", n.key),
            _ => n.key.clone(),
        };
        let events: std::collections::BTreeMap<String, Retained> = n
            .events
            .iter()
            .map(|(name, v)| {
                let r = self.frames.borrow_mut().retain(v.clone());
                acc.retained.push(r);
                (name.clone(), r)
            })
            .collect();
        // A `show` of a resource whose type a Rust app's views draw: that view's tree stands here,
        // rendered inline as its own boundary (one-app-model.md §2). What the app's view built is
        // kept as the runtime's nodes, wired to the runtime's actions, so the host draws it as it
        // draws a tilolang view and a press goes through the same door.
        if n.kind == SHOW
            && let Some(node) =
                self.rust_show(cx, n, &key, &events, walk, acc, hook.as_deref_mut())?
        {
            return Ok(node);
        }
        // A call of another package's view: its boundary renders it where it stands, and this
        // node is the fragment the boundary's tree goes in.
        if let Some(boundaries) = hook.as_deref_mut()
            && let Some(call) = self.view_call(&n.kind, &n.props, cx.offer, acc)
        {
            match call {
                Ok(call) => {
                    return Self::foreign_view(cx, n, key, events, call, walk, boundaries);
                }
                // A view call of an app whose package this runtime does not hold: the session
                // publishes it, and until it comes the call waits where it stands, so the
                // rest of the view draws. A call that failed for anything else is the
                // caller's to report.
                Err(error) if self.read_waited(&error) => {
                    let waiting = self.waiting_rendered();
                    return Ok(Self::waiting_view(n, key, events, acc, waiting));
                }
                Err(error) => return Err(error),
            }
        }
        let below = Walk::below(walk, n, key.as_str());
        let (children, rows_total) = self.build_children(cx, (n, &key), below, acc, hook)?;
        let mut props = n.props.clone();
        if below.preview.is_some() {
            // How many rows the list has, the ones not built included: the count it carries
            // when its rows are built in place ([`crate::view::LIST_LEN`]), else the rows its
            // loops made.
            props
                .entry(LIST_LEN.to_owned())
                .or_insert_with(|| Value::Int(i64::try_from(rows_total).unwrap_or(i64::MAX)));
        }
        let mut node = UiNode {
            kind: n.kind.clone(),
            key,
            props,
            binding: n.binding.clone(),
            events,
            anchor: n.anchor.clone(),
            children,
        };
        if let Some(call) = self.view_call(&n.kind, &node.props, cx.offer, acc) {
            self.resolve_foreign(cx, call?, &mut node, acc)?;
        } else if n.kind == EMBED {
            self.resolve_embed(cx, &mut node, acc);
        } else if n.kind == crate::embed::PROPOSALS {
            self.resolve_proposals(cx, &mut node, acc);
        } else {
            // The collection a loop of the node lists, whose removed items it may stand in for.
            let listed = n.children.iter().find_map(|c| {
                let child = cx.nodes.get(*c)?;
                match child.props.get(ITEMS) {
                    Some(Value::Coll(coll)) if child.kind.starts_with(FOR_EACH) => {
                        Some(Rc::clone(coll))
                    }
                    _ => None,
                }
            });
            self.mark_unseen(
                &mut node,
                (cx.offer.viewer, cx.offer.window),
                listed.as_deref(),
                walk,
                acc,
            )?;
        }
        Ok(node)
    }

    /// The fragment a call of another package's view stands for: its props without the
    /// arguments passed, and the tree its boundary renders for it.
    fn foreign_view(
        cx: &Build<'_>,
        n: &NodeBuild,
        key: String,
        events: std::collections::BTreeMap<String, Retained>,
        (vt, args): (ViewTarget, Vec<Value>),
        walk: Walk<'_>,
        boundaries: &mut dyn Boundaries,
    ) -> Result<UiNode, RuntimeError> {
        let mut props = n.props.clone();
        props.retain(|name, _| !name.starts_with(ARG_PREFIX) && name != SHOWN);
        let children = boundaries.boundary(Stub {
            key: &key,
            target: Rc::new(vt),
            args,
            lazy: walk.in_list,
            depth: cx.offer.depth.saturating_add(1),
        })?;
        Ok(UiNode {
            kind: "fragment".to_owned(),
            key,
            props,
            binding: n.binding.clone(),
            events,
            anchor: n.anchor.clone(),
            children,
        })
    }

    /// The fragment a view call waiting on another app's package stands for: its props without
    /// the arguments passed, and the toolkit's waiting state where its tree would go. The call
    /// runs again when the package arrives ([`crate::foreign::packages_key`]), which the
    /// waiting state notes as read.
    fn waiting_view(
        n: &NodeBuild,
        key: String,
        events: std::collections::BTreeMap<String, Retained>,
        acc: &mut Acc,
        waiting: Rendered,
    ) -> UiNode {
        let mut props = n.props.clone();
        props.retain(|name, _| !name.starts_with(ARG_PREFIX) && name != SHOWN);
        UiNode {
            kind: "fragment".to_owned(),
            key,
            props,
            binding: n.binding.clone(),
            events,
            anchor: n.anchor.clone(),
            children: vec![acc.absorb(waiting)],
        }
    }

    /// The children of `n`, its loops' rows in their places, and how many there are: a
    /// preview's rows not built included.
    fn build_children<'h>(
        &self,
        cx: &Build<'_>,
        (n, key): (&NodeBuild, &str),
        below: Walk<'_>,
        acc: &mut Acc,
        mut hook: Option<&mut (dyn Boundaries + 'h)>,
    ) -> Result<(Vec<UiNode>, usize), RuntimeError> {
        let mut rows_total = 0usize;
        let mut shown = 0usize;
        let mut keys = HashSet::new();
        let mut children = Vec::with_capacity(n.children.len());
        for c in &n.children {
            let rows = match cx.nodes.get(*c) {
                Some(loop_node) if loop_node.kind.starts_with(FOR_EACH) => {
                    // Rows alone among their parent's children can follow their collection.
                    let parent = (n.children.len() == 1).then_some(key);
                    let (rows, total) =
                        self.build_rows(cx, (loop_node, parent), below, acc, hook.as_deref_mut())?;
                    rows_total = rows_total.saturating_add(total);
                    rows
                }
                _ => {
                    rows_total = rows_total.saturating_add(1);
                    // A card's rows stand in the tree already when a template's list lowers
                    // them in place: it shows the first of them as a preview of a loop's rows
                    // does, and the list still counts them all.
                    let item = cx.nodes.get(*c).is_some_and(|child| child.kind == "item");
                    if item && below.preview.is_some_and(|rows| shown >= rows) {
                        continue;
                    }
                    if item {
                        shown = shown.saturating_add(1);
                    }
                    vec![self.build_node(cx, *c, below, acc, hook.as_deref_mut())?]
                }
            };
            for child in rows {
                if !keys.insert(child.key.clone()) {
                    return Err(RuntimeError::Package(format!(
                        "duplicate key `{}` among the children of `{}`",
                        child.key, n.key
                    )));
                }
                children.push(child);
            }
        }
        Ok((children, rows_total))
    }

    /// The rows of a loop's node, in its place among its parent's children: per item, a
    /// `fragment` keyed by the item that stands for the row view called on it. With
    /// `boundaries` each row is a boundary of its own (rendered when shown, for a row of a
    /// `list`); without, each is rendered here and its reads join this view's. In a preview
    /// only the rows it shows are built. Returns them and how many items the loop has.
    ///
    /// A loop over a collection that is the only child of `parent` hands its rows to the
    /// boundaries ([`Boundaries::rows`]): they follow the collection's inserts and removals
    /// from then on, and the view holding the loop does not depend on which items it has.
    fn build_rows<'h>(
        &self,
        cx: &Build<'_>,
        (n, parent): (&NodeBuild, Option<&str>),
        walk: Walk<'_>,
        acc: &mut Acc,
        mut hook: Option<&mut (dyn Boundaries + 'h)>,
    ) -> Result<(Vec<UiNode>, usize), RuntimeError> {
        let target = n.kind.strip_prefix(FOR_EACH).unwrap_or_default();
        let key = match walk.prefix {
            Some(prefix) => format!("{prefix}~{}", n.key),
            None => n.key.clone(),
        };
        let items = match n.props.get(ITEMS) {
            Some(Value::List(items)) => Items::List(items),
            Some(Value::Coll(coll)) => Items::Coll(
                coll,
                self.coll_listing(coll).map_err(RuntimeError::Package)?,
            ),
            _ => {
                return Err(RuntimeError::Package(format!(
                    "the loop `{}` has no list of items",
                    n.key
                )));
            }
        };
        let suffix = match n.props.get(SUFFIX) {
            Some(Value::Text(suffix)) => suffix.clone(),
            _ => String::new(),
        };
        let (vt, args) = self.rows_call(target, &n.props)?;
        // The sizing and placement the compiler took out of a one-node body: each row's.
        let layout: std::collections::BTreeMap<String, Value> = n
            .props
            .iter()
            .filter(|(name, _)| {
                ![ITEMS, SUFFIX, EACH].contains(&name.as_str()) && !name.starts_with(ARG_PREFIX)
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let template = RowsOf {
            key,
            suffix,
            target: Rc::new(vt),
            args,
            layout,
            lazy: walk.in_list,
            depth: cx.offer.depth.saturating_add(1),
        };
        let total = items.len();
        // The rows of a `list`, alone in it, are virtual: none is built until the host shows
        // it, so the list costs the rows on screen, not its length.
        if walk.in_list
            && walk.preview.is_none()
            && let (Some(list), Some(boundaries)) = (parent, hook.as_deref_mut())
            && boundaries.virtualizes()
        {
            let source = match &items {
                Items::Coll(coll, _) => {
                    boundaries.rows(LoopRows {
                        coll: Rc::clone(coll),
                        resource: coll.target.id_string(),
                        parent: list.to_owned(),
                        rows: template.clone(),
                    });
                    RowSource::Coll(Rc::clone(coll))
                }
                Items::List(values) => RowSource::List(Rc::from(*values)),
            };
            let rows = boundaries.virtual_rows(VirtualRows {
                list: list.to_owned(),
                rows: template,
                source,
            })?;
            return Ok((rows, total));
        }
        let shown = walk.preview.map_or(total, |rows| rows.min(total));
        // The rows depend on the collection's items: in a preview, on its length and the items
        // it shows; for rows the boundaries keep, on nothing here; else on all of them.
        if let Items::Coll(coll, listing) = &items {
            if walk.preview.is_some() {
                acc.colls.push(CollSeen::shown(coll, listing, shown));
            } else if let (Some(parent), Some(boundaries)) = (parent, hook.as_deref_mut()) {
                boundaries.rows(LoopRows {
                    coll: Rc::clone(coll),
                    resource: coll.target.id_string(),
                    parent: parent.to_owned(),
                    rows: template.clone(),
                });
            } else if coll.derived() {
                // Which items a filtered collection has is no field of it: the rows depend on
                // each of them.
                acc.colls.push(CollSeen::shown(coll, listing, total));
            } else {
                acc.reads
                    .push((coll.target.id_string(), coll.field.clone()));
            }
        }
        let mut rows = Vec::with_capacity(shown);
        for i in 0..shown {
            let Some(item) = items.get(self, i) else {
                break;
            };
            let (row_key, args) = template.row(&item, i);
            let children = if let Some(boundaries) = hook.as_deref_mut() {
                boundaries.boundary(template.stub(&row_key, args))?
            } else {
                let nested = self.render_at(&template.target, &args, cx.offer.deeper(), None)?;
                vec![scope(&row_key, acc.absorb(nested))]
            };
            rows.push(template.fragment(row_key, children));
        }
        Ok((rows, total))
    }

    /// The view a loop's node calls per row and what it passes each parameter: the item to
    /// the one the node's `each` names, the row's key suffix to `$key`, and the node's
    /// `arg.<name>` properties to the rest.
    fn rows_call(
        &self,
        target: &str,
        props: &std::collections::BTreeMap<String, Value>,
    ) -> Result<(ViewTarget, Vec<RowArg>), RuntimeError> {
        let (app, view) = target
            .split_once(':')
            .ok_or_else(|| RuntimeError::Package(format!("malformed loop node `{target}`")))?;
        let vt = self.view_target(view)?;
        let own = self
            .registry
            .borrow()
            .package(&vt.module)
            .is_some_and(|pkg| pkg.meta.app == app);
        if !own {
            return Err(RuntimeError::Package(format!(
                "the row view {view} does not belong to app `{app}`"
            )));
        }
        let each = match props.get(EACH) {
            Some(Value::Text(each)) => each.as_str(),
            _ => vt.params.first().map_or("", |p| p.name.as_str()),
        };
        let around = vt
            .params
            .iter()
            .map(|p| {
                if p.name == each {
                    return Ok(RowArg::Item);
                }
                if p.name == ROW_KEY {
                    return Ok(RowArg::Key);
                }
                props
                    .get(&format!("{ARG_PREFIX}{}", p.name))
                    .cloned()
                    .map(RowArg::Value)
                    .ok_or_else(|| {
                        RuntimeError::Package(format!(
                            "the loop node for `{view}` is missing the value `{}`",
                            p.name
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((vt, around))
    }

    /// The view a node of kind `kind` calls and its arguments, when it calls one: another
    /// package's view by name (`view:<app>:<view id>`), or a resource's type's default view
    /// (`show(r)`).
    fn view_call(
        &self,
        kind: &str,
        props: &std::collections::BTreeMap<String, Value>,
        offer: Offer<'_>,
        acc: &mut Acc,
    ) -> Option<Result<(ViewTarget, Vec<Value>), RuntimeError>> {
        if let Some(target) = kind.strip_prefix(FOREIGN_VIEW) {
            return Some(self.call_of(target, props));
        }
        (kind == SHOW).then(|| self.shown_view(props, offer, acc))
    }

    /// The view a `show` node draws its resource through, called on it: the one its `view`
    /// names (an offered view of the resource's package that takes the resource alone, by id or
    /// name); else the view the person chose for it in the window's bar
    /// ([`chosen_view`](Self::chosen_view)); else its type's default view. A type without a
    /// default view is an error: a shell asks `meta(r).opens` before it shows a resource. The
    /// view is its own package's code wherever it is drawn, another app's
    /// (`Runtime::load_foreign_package`) included.
    fn shown_view(
        &self,
        props: &std::collections::BTreeMap<String, Value>,
        offer: Offer<'_>,
        acc: &mut Acc,
    ) -> Result<(ViewTarget, Vec<Value>), RuntimeError> {
        let Some(Value::Ref(shown)) = props.get(SHOWN) else {
            return Err(RuntimeError::Package(
                "`show` was given no resource to draw".into(),
            ));
        };
        let ty = &shown.target.ty;
        let named = match props.get(SHOWN_AS) {
            Some(Value::Text(view)) if !view.is_empty() => Some(view.as_str()),
            _ => None,
        };
        // A flag a view wrote as `row: true` reaches the node as `1`, as every flag does.
        let row = match props.get(SHOWN_AS_ROW) {
            Some(Value::Bool(flag)) => *flag,
            Some(Value::Int(n)) => *n != 0,
            _ => false,
        };
        let chosen = match named {
            Some(_) => None,
            None if row => None,
            None => self.chosen_view(shown, offer, acc),
        };
        let view = {
            let registry = self.registry.borrow();
            match (named, row) {
                (Some(view), _) => registry.view_of_type(ty, view).ok_or_else(|| {
                    RuntimeError::Package(format!(
                        "`show`: `{ty}` has no view `{view}` that takes it alone"
                    ))
                })?,
                (None, true) => registry.row_view(ty).ok_or_else(|| {
                    RuntimeError::Package(format!(
                        "`show`: the type `{ty}` declares no row view to draw it as a row"
                    ))
                })?,
                (None, false) => match chosen {
                    Some(view) => view,
                    None => registry.default_view(ty).ok_or_else(|| {
                        RuntimeError::Package(format!(
                            "`show`: the type `{ty}` has no default view to draw it with"
                        ))
                    })?,
                },
            }
        };
        let vt = self.view_target(&view)?;
        if vt.params.len() != 1 {
            return Err(RuntimeError::Package(format!(
                "`show`: the view {view} of `{ty}` takes {} arguments, not the resource alone",
                vt.params.len()
            )));
        }
        Ok((vt, vec![Value::Ref(shown.clone())]))
    }

    /// A `show` of a resource whose type a Rust app draws: that view's tree, made the runtime's and
    /// keyed where the `show` node stands, so the host builds it as it builds a tilolang view.
    /// `None` when no Rust app draws the resource's type, so the node resolves as a package's view
    /// does.
    fn rust_show(
        &self,
        cx: &Build<'_>,
        n: &NodeBuild,
        key: &str,
        events: &std::collections::BTreeMap<String, Retained>,
        _walk: Walk<'_>,
        acc: &mut Acc,
        _hook: Option<&mut (dyn Boundaries + '_)>,
    ) -> Result<Option<UiNode>, RuntimeError> {
        let Some(Value::Ref(shown)) = n.props.get(SHOWN) else {
            return Ok(None);
        };
        let views = crate::rust_views::app_of(&self.rust_views, &shown.target.ty);
        let Some(app) = views else {
            return Ok(None);
        };
        let named = match n.props.get(SHOWN_AS) {
            Some(Value::Text(view)) if !view.is_empty() => view.clone(),
            _ => String::new(),
        };
        let Some(decl) = crate::rust_views::view_for(&app, &shown.target.ty, &named) else {
            return Ok(None);
        };
        let kind = crate::rust_views::kind_of(&app, &shown.target.ty);
        let resource = crate::rust_views::RustResource {
            id: shown.target.id_string(),
            kind: kind.clone(),
        };
        // The reading, whole, and the slot that moves when it does, so the boundary runs again.
        let reading = self.foreign.borrow_mut().whole(&shown.target);
        if let Some((_, generation)) = &reading {
            let state_key = crate::foreign::reading_key(&shown.target);
            acc.state_reads.push((state_key.clone(), *generation));
            if let Some(seen) = self.view_states.borrow().get(&state_key).cloned() {
                acc.state_seen.push((state_key, seen));
            }
        }
        let rendered = app.render(
            &kind,
            &decl.name,
            &resource,
            reading.as_ref().map(|(json, _)| json),
            cx.offer.viewer,
            cx.offer.window,
        );
        let author = crate::ids::ModuleHash::principal(app.app());
        let root = {
            let mut retained = Vec::new();
            let mut retain = |value: Value| {
                let r = self.frames.borrow_mut().retain(value);
                retained.push(r);
                r
            };
            let root = rendered
                .tree
                .into_ui(
                    &rendered.events,
                    &shown.target,
                    &author,
                    cx.offer.window,
                    cx.offer.viewer,
                    &mut retain,
                )
                .map_err(RuntimeError::Package)?;
            acc.retained.extend(retained);
            root
        };
        let mut node = UiNode {
            kind: "fragment".to_owned(),
            key: key.to_owned(),
            props: std::collections::BTreeMap::new(),
            binding: n.binding.clone(),
            events: events.clone(),
            anchor: n.anchor.clone(),
            children: vec![scope(key, root)],
        };
        node.props
            .retain(|name, _| !name.starts_with(ARG_PREFIX) && name != SHOWN);
        Ok(Some(node))
    }

    /// The view a window's own boundary shows its resource through, and the state it read to
    /// know: the one the bar's view switch chose among the views of the resource's type, when
    /// there is one and it is not the view the window opened on. `None` when the type offers a
    /// single view or the person chose none; the state is still returned for such a type, so
    /// that choosing one runs the window again.
    fn window_view(
        &self,
        target: &ViewTarget,
        args: &[Value],
        offer: Offer<'_>,
    ) -> (
        Option<ViewTarget>,
        Option<(crate::view_state::StateKey, u64, Value)>,
    ) {
        let Some(Value::Ref(resource)) = args.first() else {
            return (None, None);
        };
        let views = {
            let registry = self.registry.borrow();
            registry.views_of_type(&resource.target.ty)
        };
        if views.len() < 2 {
            return (None, None);
        }
        let key = StateKey::view(
            offer.window,
            offer.viewer.0.clone(),
            format!("{VIEW_STATE}{}", resource.target.id_string()),
        );
        let (held, generation) = {
            let states = self.view_states.borrow();
            (states.get(&key).cloned(), states.generation(&key))
        };
        let seen = held.clone().unwrap_or_else(Value::none);
        let chosen = if let Some(Value::Int(index)) = held
            && let Ok(index) = usize::try_from(index)
            && let Some((id, _)) = views.get(index)
            && *id != target.view
        {
            self.view_target(id).ok()
        } else {
            None
        };
        (chosen, Some((key, generation, seen)))
    }

    /// The view a `show` node draws its resource through: the one a person chose in the bar's
    /// view switch (the tab of [`surfaces`](Core::surfaces), kept in the view state slot
    /// [`VIEW_STATE`] names for the resource, read here so the window runs again when it
    /// changes). `None` while nobody chose, or the choice names a view the resource's type does
    /// not offer any more.
    fn chosen_view(&self, resource: &RefValue, offer: Offer<'_>, acc: &mut Acc) -> Option<ViewId> {
        let key = StateKey::view(
            offer.window,
            offer.viewer.0.clone(),
            format!("{VIEW_STATE}{}", resource.target.id_string()),
        );
        let held = {
            let states = self.view_states.borrow();
            let held = states.get(&key).cloned();
            acc.state_reads.push((key.clone(), states.generation(&key)));
            acc.state_seen
                .push((key, held.clone().unwrap_or_else(Value::none)));
            held
        };
        let Some(Value::Int(index)) = held else {
            return None;
        };
        let index = usize::try_from(index).ok()?;
        let views = { self.registry.borrow().views_of_type(&resource.target.ty) };
        views.get(index).map(|(id, _)| id.clone())
    }

    /// The view a `view:<app>:<view id>` node applies and its arguments, from the node's
    /// `arg.<name>` properties. A view of an app whose package this runtime does not hold is
    /// the ask for that package, and the call waits on it ([`Core::missing_view`]).
    pub(crate) fn call_of(
        &self,
        target: &str,
        props: &std::collections::BTreeMap<String, Value>,
    ) -> Result<(ViewTarget, Vec<Value>), RuntimeError> {
        let (app, view) = target
            .split_once(':')
            .ok_or_else(|| RuntimeError::Package(format!("malformed view node `{target}`")))?;
        let vt = self
            .view_target(view)
            .map_err(|_: RuntimeError| self.missing_view(app, view))?;
        let pkg: std::rc::Rc<Package> = self
            .registry
            .borrow()
            .package(&vt.module)
            .ok_or_else(|| RuntimeError::NotFound(format!("module {}", vt.module)))?;
        if pkg.meta.app != app {
            return Err(RuntimeError::Package(format!(
                "view {view} belongs to app `{}`, not `{app}`",
                pkg.meta.app
            )));
        }
        let args = vt
            .params
            .iter()
            .map(|p| {
                props
                    .get(&format!("{ARG_PREFIX}{}", p.name))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::Package(format!(
                            "the view node for `{view}` is missing the argument `{}`",
                            p.name
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((vt, args))
    }

    /// Replace a node applying a view with a `fragment` holding that view's tree, rendered in
    /// place: its reads join the outer view's. The nested view's code is its own package's,
    /// so its action references are authored by that package.
    fn resolve_foreign(
        &self,
        cx: &Build<'_>,
        (vt, args): (ViewTarget, Vec<Value>),
        node: &mut UiNode,
        acc: &mut Acc,
    ) -> Result<(), RuntimeError> {
        node.props.remove(SHOWN);
        let nested = self.render_at(&vt, &args, cx.offer.deeper(), None)?;
        let root = acc.absorb(nested);
        "fragment".clone_into(&mut node.kind);
        node.props.retain(|name, _| !name.starts_with(ARG_PREFIX));
        node.children = vec![scope(&node.key, root)];
        Ok(())
    }

    // ----- blocks ------------------------------------------------------------------------

    /// Compile and register the module of a block.
    ///
    /// # Errors
    ///
    /// `Package` for a module that does not link.
    pub fn load_block(
        &self,
        wasm: &[u8],
        meta: &tilolang::PackageMeta,
    ) -> Result<std::rc::Rc<Package>, RuntimeError> {
        let hash = self.engine.compile(wasm)?;
        let pkg = Package::new(hash, meta.clone()).map_err(RuntimeError::Package)?;
        self.registry
            .borrow_mut()
            .add(pkg)
            .map_err(RuntimeError::Package)
    }

    /// Run a block's `b_main`, which takes no arguments, and read its value.
    pub fn eval_block(
        &self,
        pkg: &Package,
        value_type: &str,
        author: Option<&ModuleHash>,
    ) -> Result<Value, RuntimeError> {
        self.frames
            .borrow_mut()
            .push(author.unwrap_or(&pkg.hash).clone(), FrameKind::Pure);
        let out = self.call_in_frame(pkg, "b_main", &[], &[], Some(value_type));
        self.frames.borrow_mut().pop();
        out
    }
}

/// The items a loop's node holds: a list of values, or a collection read item by item.
enum Items<'a> {
    List(&'a [Value]),
    Coll(
        &'a std::rc::Rc<crate::value::CollRef>,
        std::rc::Rc<crate::store::Listing>,
    ),
}

impl Items<'_> {
    fn len(&self) -> usize {
        match self {
            Self::List(items) => items.len(),
            Self::Coll(_, listing) => listing.len(),
        }
    }

    /// The `index`th item, as a value.
    fn get(&self, core: &Core, index: usize) -> Option<Value> {
        match self {
            Self::List(items) => items.get(index).cloned(),
            Self::Coll(coll, listing) => listing.get(index).map(|id| core.item_ref(coll, &id)),
        }
    }
}

/// What keys the row of `item`, the `index`th of its loop: a resource by its id, any other
/// value by its place.
pub(crate) fn item_key(item: &Value, index: usize) -> String {
    match item {
        Value::Ref(r) => r.target.id_string(),
        _ => index.to_string(),
    }
}

/// How a loop makes its rows: the row view, what each of its parameters takes, and the key
/// and layout of each row's `fragment`.
#[derive(Clone)]
pub(crate) struct RowsOf {
    /// The loop's key: each row's is `<key>/<item key>`.
    key: String,
    /// The key suffix of the rows around the loop.
    suffix: String,
    target: Rc<ViewTarget>,
    args: Vec<RowArg>,
    layout: std::collections::BTreeMap<String, Value>,
    /// Rows of a `list`: rendered when the host shows them.
    pub lazy: bool,
    pub depth: usize,
}

impl RowsOf {
    /// The key of the row whose item is keyed `item_key` (see [`item_key`]).
    pub fn key_of(&self, item_key: &str) -> String {
        format!("{}/{item_key}", self.key)
    }

    /// The key of the row of the item `id` of `coll`, without a reference to it: the item's
    /// as its reference writes it.
    pub fn coll_row_key(&self, coll: &crate::value::CollRef, id: &str) -> String {
        format!("{}/{}/{id}", self.key, coll.target.doc)
    }

    /// The key of the row of `item`, the `index`th, and the row view's arguments.
    pub fn row(&self, item: &Value, index: usize) -> (String, Vec<Value>) {
        let item_key = item_key(item, index);
        let row_key = format!("{}/{item_key}", self.key);
        let args = self
            .args
            .iter()
            .map(|arg| match arg {
                RowArg::Item => item.clone(),
                RowArg::Key => Value::Text(format!("{}/{item_key}", self.suffix)),
                RowArg::Value(value) => value.clone(),
            })
            .collect();
        (row_key, args)
    }

    /// Whether `other` makes its rows as this does: the same view of the same items, with the
    /// same arguments and layout. Rows built from this template stand for `other`'s too.
    pub fn makes_as(&self, other: &Self) -> bool {
        self.key == other.key
            && self.suffix == other.suffix
            && self.target.module == other.target.module
            && self.target.export == other.target.export
            && self.args == other.args
            && self.layout == other.layout
            && self.lazy == other.lazy
            && self.depth == other.depth
    }

    /// The boundary of the row keyed `key`, called with `args`.
    pub fn stub<'k>(&self, key: &'k str, args: Vec<Value>) -> Stub<'k> {
        Stub {
            key,
            target: Rc::clone(&self.target),
            args,
            lazy: self.lazy,
            depth: self.depth,
        }
    }

    /// The `fragment` that stands for the row keyed `key` in the tree.
    pub fn fragment(&self, key: String, children: Vec<UiNode>) -> UiNode {
        UiNode {
            kind: "fragment".to_owned(),
            key,
            props: self.layout.clone(),
            binding: None,
            events: std::collections::BTreeMap::new(),
            anchor: None,
            children,
        }
    }
}

/// The rows of a loop over a collection, alone among the children of the node keyed
/// `parent`: kept by the boundaries, which apply the collection's inserts and removals to
/// them.
pub(crate) struct LoopRows {
    pub coll: Rc<crate::value::CollRef>,
    /// The collection's resource, as [`crate::value::ResourceRef::id_string`] writes it.
    pub resource: String,
    pub parent: String,
    pub rows: RowsOf,
}

impl LoopRows {
    /// The key of the row of the item `id` and the row view's arguments.
    pub fn row(&self, core: &Core, id: &str) -> (String, Vec<Value>) {
        self.rows.row(&core.item_ref(&self.coll, id), 0)
    }

    /// The key of the row of the item `id`, without a reference to it.
    pub fn row_key(&self, id: &str) -> String {
        self.rows.coll_row_key(&self.coll, id)
    }
}

/// What a row view's parameter takes.
#[derive(Clone, PartialEq)]
enum RowArg {
    Item,
    /// The row's key suffix.
    Key,
    Value(Value),
}
