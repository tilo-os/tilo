//! The **retained accessibility tree** — a stable, frame-to-frame semantic
//! model of the running UI.
//!
//! The toolkit is immediate-mode: `draw` and
//! [`accesskit_bridge::build_tree_update`](crate::accesskit_bridge::build_tree_update)
//! both re-derive a fresh tree each call, which is fine for pushing a whole
//! tree at AccessKit but gives no stable identity or diff — nothing survives
//! between frames. A [`SemanticTree`] is the retained counterpart: it keys
//! nodes by a stable [`SemanticId`] and, on each [`refresh`](SemanticTree::refresh),
//! diffs the freshly-walked UI against the previous frame, returning a
//! [`TreeDelta`] while preserving identity for every node that recurs. This
//! is the foundation an AT-SPI incremental push, a screen reader's change
//! narration, or an agent/CLI introspection layer builds on.
//!
//! Each refresh assembles the tree from two sources, so a node carries both
//! what it is and what you can do to it: the
//! [`Widget::accessibility`] walk
//! (roles/names/values/state, collapsing presentational widgets like the
//! AccessKit bridge does) contributes generic verbs (focus, activate, …);
//! the app's [`resources()`](crate::widget::Widget::resources) forest from
//! the Tilo [action model](tilo_actions) contributes
//! [`SemanticActionKind::App`] affordances carrying the exact
//! [`EncodedAction`] the click action sends —
//! so invoking one by name drives the same replayable action through the
//! one audited dispatch path, letting an assistive client operate the tree,
//! not just read it.
//!
//! The tree itself imports no AccessKit and no D-Bus — pure data plus a
//! diff, unit-testable without a bus or a window.

use std::collections::HashMap;
use std::sync::Arc;

use taffy::NodeId as TaffyNodeId;
use vello::kurbo::Rect;

use crate::a11y::{AccessNode, AccessState, Action, Role, Sensitivity};
use crate::widget::Widget;

#[cfg(test)]
#[path = "a11y_tree/identity_tests.rs"]
mod identity_tests;

#[cfg(test)]
#[path = "a11y_tree/continuation_tests.rs"]
mod continuation_tests;

#[path = "a11y_tree/continuation.rs"]
mod continuation;

#[path = "a11y_tree/node_identity.rs"]
mod node_identity;
pub(crate) use node_identity::AdmittedNodeNames;
use node_identity::WidgetNodeMap;

type HostedWidget<'a> = (Vec<usize>, &'a dyn Widget);
type HostedByIndex<'a> = HashMap<usize, Vec<HostedWidget<'a>>>;
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionInput, ActionSpec, Collects, EncodedAction, OperationKind, Resource};

/// Operation type used when the retained semantic tree projects an ordinary
/// UI affordance into the resource/action catalogue. This makes the same
/// control addressable from Ctrl+K, the automation socket, and agents without
/// inventing an app-specific command bus.
pub(crate) const SEMANTIC_OPERATION_TYPE: &str = "tilo.ui.semantic-action";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SemanticOperation {
    pub node: String,
    pub action: String,
    pub value: Option<String>,
}

/// Stable identity of a semantic node, preserved across [`refresh`](SemanticTree::refresh)
/// and across structural rebuilds of the widget tree as long as the backing
/// widget or resource recurs.
///
/// The string form encodes the node's origin, in order of precedence, so ids
/// never collide between the widget walk and the resource forest. A layout
/// (Taffy) node id is never part of it: those are arena slots reassigned on
/// every rebuild.
/// * `"root"` — the synthetic window root.
/// * `"r:<resource_id>"` — a resource from the app's action model, keyed by its
///   already-stable [`ResourceId`](tilo_actions::ResourceId).
/// * `"t:<test_id>"` — a widget whose [`Widget::test_id`] is unique in the live
///   tree. A test id shared by several widgets identifies none of them and
///   falls through to the forms below.
/// * `"<parent>~k:<key>"` — a widget with a declared
///   [`semantic_key`](crate::a11y::AccessNode::semantic_key), or a synthetic
///   child with an [`AccessNode::key`](crate::a11y::AccessNode::key), keyed
///   within its semantic parent.
/// * `"<parent>~<index>"` — a synthetic a11y child (a table row/cell) with no
///   key, keyed by its position under its parent.
/// * `"<anchor>/<Name>[<index>]/…"` — any other widget: its widget-tree path
///   from the nearest ancestor above that has one of the ids above, in the
///   headless driver's `Parent/Child[index]` form (`index` counts same-named
///   siblings). With no such ancestor the id is exactly the headless path
///   (`Column[0]/Button[1]`), so a `tilo-control` target and a semantic id
///   name the same widget.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SemanticId(pub String);

impl SemanticId {
    pub(crate) fn root() -> Self {
        SemanticId("root".into())
    }
    pub(crate) fn test_id(id: &str) -> Self {
        SemanticId(format!("t:{id}"))
    }
    pub(crate) fn synthetic(parent: &SemanticId, index: usize) -> Self {
        SemanticId(format!("{}~{}", parent.0, index))
    }
    pub(crate) fn keyed(parent: &SemanticId, key: impl std::fmt::Display) -> Self {
        SemanticId(format!("{}~k:{}", parent.0, key))
    }
    fn resource(id: &str) -> Self {
        SemanticId(format!("r:{id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a [`SemanticAction`] is carried out when invoked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticActionKind {
    /// A generic accessibility verb ([`Action`]) — focus, activate,
    /// increment/decrement, expand/collapse, set-value. Routed through
    /// [`Widget::perform_action`] with the
    /// `App`'s generic focus/click fallback, exactly like an AccessKit
    /// `ActionRequest`.
    Verb(Action),
    /// An app action from the Tilo [action model](tilo_actions), ready to fire.
    /// `encoded` is the byte-identical [`EncodedAction`] the on-screen affordance
    /// and the Ctrl+K palette dispatch — invoking it replays the *same* action.
    App(EncodedAction),
    /// Inert address of a no-argument creation; only the current owner encodes it.
    AppDeferred {
        address: tilo_actions::ActionAddress,
    },
    /// An app action that first needs one free-text argument (a rename, a "new
    /// page named…"). The argument is supplied to
    /// [`SemanticTree::build_prompted`] to produce the concrete [`EncodedAction`].
    AppPrompted {
        /// The prompt shown when collecting the argument.
        prompt: String,
        /// Placeholder text for the argument field.
        placeholder: String,
        /// Whether that argument names words somebody says or the address of
        /// content that already exists. This tree's audience is callers with no
        /// screen: offered a plain text field for a [`Collects::Content`]
        /// argument, one can only type something that was never going to resolve.
        collects: Collects,
    },
}

/// One thing an assistive client can do to a [`SemanticNode`], addressable by a
/// stable [`name`](Self::name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticAction {
    /// Stable, machine-addressable verb name (`"focus"`, `"activate"`,
    /// `"delete"`, `"new-page"`). Unique within a node. This is the handle an
    /// AT / CLI / agent invokes.
    pub name: String,
    /// Human, localisable label (`"Delete page"`).
    pub label: String,
    /// How the action is carried out.
    pub kind: SemanticActionKind,
}

/// One node of the retained semantic tree.
///
/// Not `Eq` because [`bounds`](Self::bounds) holds `f64`; content equality (used
/// by the diff) is a bit-exact [`PartialEq`], which is what we want — an
/// unchanged widget re-reports identical bounds.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticNode {
    pub id: SemanticId,
    /// Whether this identity is safe to persist outside the current retained
    /// frame. Structural paths and positional synthetic ids are deliberately
    /// not durable: they survive a rebuild of the same structure, but a
    /// sibling inserted before them shifts them, and exposing them to agents
    /// would let a later invocation target a different control.
    pub durable_identity: bool,
    pub role: Role,
    /// Already i18n-resolved.
    pub name: Option<String>,
    /// A field's contents, a slider's readout, etc. Never populated for a
    /// [`Secret`](Sensitivity::Secret) node.
    pub value: Option<String>,
    /// How far this node's presented content may travel.
    pub sensitivity: Sensitivity,
    pub state: AccessState,
    /// `None` for the root and resource nodes. Virtual rows and cells publish
    /// their viewport geometry even though they have no retained widget.
    /// Window (surface) logical coordinates, already scrolled.
    pub bounds: Option<Rect>,
    /// The intersection of every ancestor's child clip (a scroll viewport, a
    /// clipped panel). `None` means unclipped. What of [`bounds`](Self::bounds)
    /// lies outside it is not on screen.
    pub clip: Option<Rect>,
    pub actions: Vec<SemanticAction>,
    /// In reading order.
    pub children: Vec<SemanticId>,
    /// So the host can route a [`Verb`](SemanticActionKind::Verb) back to
    /// the retained widget that owns this node. A frame-local routing
    /// address, never identity.
    pub widget_taffy_id: Option<u64>,
    /// Opaque widget-local target for a synthetic child action.
    pub semantic_target: Option<String>,
    /// The widget's declared [control key](crate::a11y::AccessNode::semantic_key),
    /// which control bindings name.
    pub control_key: Option<String>,
    /// The document content this node stands for: the resource itself for a
    /// resource node, the declared [`resource`](crate::a11y::AccessNode::resource)
    /// resolved against the app's resource forest for a widget.
    pub reference: Option<tilo_reference::ResourceRef>,
    /// The node paints feedback of this device's own clock and state; see
    /// [`AccessNode::transient`](crate::a11y::AccessNode::transient).
    pub transient: bool,
}

impl SemanticNode {
    pub fn action(&self, name: &str) -> Option<&SemanticAction> {
        self.actions.iter().find(|a| a.name == name)
    }

    fn same_exported_content(&self, other: &Self) -> bool {
        // `widget_taffy_id` is a frame-local routing address. A keyed widget can
        // be rebound to a new layout node without changing anything exposed to
        // semantic-tree consumers; the freshly stored node still carries the
        // new address for action dispatch.
        self.id == other.id
            && self.durable_identity == other.durable_identity
            && self.role == other.role
            && self.name == other.name
            && self.value == other.value
            && self.sensitivity == other.sensitivity
            && self.state == other.state
            && self.bounds == other.bounds
            && self.clip == other.clip
            && self.actions == other.actions
            && self.children == other.children
            && self.semantic_target == other.semantic_target
            && self.control_key == other.control_key
            && self.reference == other.reference
            && self.transient == other.transient
    }

    /// The part of [`bounds`](Self::bounds) that is on screen, or `None` when
    /// the node has no geometry or is clipped away entirely.
    pub fn visible_bounds(&self) -> Option<Rect> {
        let bounds = self.bounds?;
        let visible = match self.clip {
            Some(clip) => bounds.intersect(clip),
            None => bounds,
        };
        (visible.width() > 0.0 && visible.height() > 0.0).then_some(visible)
    }

    /// Whether [`value`](Self::value) is where this node presents its content,
    /// so its name is only a label.
    pub fn presents_value(&self) -> bool {
        crate::a11y::presents_value(self.role, self.value.is_some())
    }

    /// The window itself. Its bounds are the whole surface: the toolkit
    /// paints every pixel of it (background, padding, chrome), so the
    /// window's pixels are described even where no widget node lies.
    fn window_root(children: Vec<SemanticId>, surface: Option<Rect>) -> Self {
        SemanticNode {
            id: SemanticId::root(),
            durable_identity: true,
            role: Role::Window,
            name: Some("Tilo application window".into()),
            value: None,
            sensitivity: Sensitivity::Public,
            state: AccessState::default(),
            bounds: surface,
            clip: None,
            actions: Vec::new(),
            children,
            widget_taffy_id: None,
            semantic_target: None,
            control_key: None,
            reference: None,
            transient: false,
        }
    }
}

/// What changed between two [`refresh`](SemanticTree::refresh)es. Ids that appear
/// in neither list are present and byte-identical in both frames — their identity
/// *and* content survived unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeDelta {
    /// Ids present this frame but not last frame.
    pub added: Vec<SemanticId>,
    /// Ids present last frame but not this frame.
    pub removed: Vec<SemanticId>,
    /// Ids present in both frames whose content (role / name / value / state /
    /// bounds / actions / children) differs.
    pub updated: Vec<SemanticId>,
}

impl TreeDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.updated.is_empty()
    }
}

/// Build closures for [`AppPrompted`](SemanticActionKind::AppPrompted)
/// actions, keyed by `(node id, action name)`.
type PromptedMap = HashMap<(SemanticId, String), Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>>;

/// The retained accessibility tree. Persists node identity across frames and
/// diffs each [`refresh`](Self::refresh) against the previous.
pub struct SemanticTree {
    nodes: HashMap<SemanticId, SemanticNode>,
    /// DFS reading order, so consumers can iterate the tree deterministically.
    order: Vec<SemanticId>,
    root: SemanticId,
    /// Build closures for [`AppPrompted`](SemanticActionKind::AppPrompted)
    /// actions, keyed by `(node id, action name)`. Kept out of [`SemanticNode`]
    /// (a closure is neither `PartialEq` nor `Clone`-comparable) so nodes stay
    /// diffable; refreshed wholesale each frame.
    prompted: PromptedMap,
    bound_actions: HashMap<(SemanticId, String), crate::bound_dispatch::BoundActionTarget>,
    duplicate_ids: Vec<SemanticId>,
    /// Toolkit-owned overlays open this frame, each named by its owner's id.
    overlays: Vec<crate::view::OpenOverlay>,
    /// Bumped by every refresh that changed the tree, whoever asked for it.
    revision: u64,
    /// The surface the window is laid out on, in logical pixels: the
    /// window root's bounds.
    surface: Option<Rect>,
}

impl Default for SemanticTree {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticTree {
    pub fn new() -> Self {
        let root = SemanticId::root();
        let mut nodes = HashMap::new();
        nodes.insert(root.clone(), SemanticNode::window_root(Vec::new(), None));
        Self {
            nodes,
            order: vec![root.clone()],
            root,
            prompted: HashMap::new(),
            bound_actions: HashMap::new(),
            duplicate_ids: Vec::new(),
            overlays: Vec::new(),
            revision: 0,
            surface: None,
        }
    }

    /// The surface the window is laid out on, in logical pixels, taken as
    /// the window root's bounds from the next refresh on. `None` before the
    /// first layout.
    pub fn set_surface(&mut self, surface: Option<Rect>) {
        self.surface = surface.filter(|rect| rect.width() > 0.0 && rect.height() > 0.0);
    }

    /// Rebuild the tree from the live UI and diff it against the retained frame.
    ///
    /// `node_map` is used *read-only* (never dereferenced) to assign each
    /// widget its stable id. Ambiguous registrations remain structural and
    /// receive no layout dispatch id. `focused` folds
    /// into each node's [`AccessState::focused`]. `resources` is the focused
    /// app's open-resource forest
    /// ([`Widget::resources`]); each
    /// exported [`ActionSpec`] becomes an invokable
    /// affordance.
    pub fn refresh(
        &mut self,
        root: &dyn Widget,
        node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
        focused: Option<TaffyNodeId>,
        resources: &[Resource],
    ) -> TreeDelta {
        self.refresh_impl::<false>(root, node_map, None, focused, resources, &[], None)
    }
    pub(crate) fn refresh_bound(
        &mut self,
        root: &dyn Widget,
        node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
        admitted: &AdmittedNodeNames,
        focused: Option<TaffyNodeId>,
        resources: &[Resource],
    ) -> TreeDelta {
        self.refresh_impl::<true>(
            root,
            node_map,
            Some(admitted),
            focused,
            resources,
            &[],
            None,
        )
    }
    pub(crate) fn refresh_components(
        &mut self,
        root: &dyn Widget,
        node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
        admitted: &AdmittedNodeNames,
        focused: Option<TaffyNodeId>,
        resources: &[Resource],
        components: &[crate::bound_dispatch::BoundResourceSnapshot],
        doc: Option<&tilo_actions::Doc>,
    ) -> TreeDelta {
        self.refresh_impl::<true>(
            root,
            node_map,
            Some(admitted),
            focused,
            resources,
            components,
            doc,
        )
    }
    #[cfg(test)]
    pub(crate) fn bound_actions_for_test(
        &self,
    ) -> Vec<(SemanticId, crate::bound_dispatch::BoundActionTarget)> {
        self.bound_actions
            .iter()
            .map(|((id, _), target)| (id.clone(), target.clone()))
            .collect()
    }
    pub(crate) fn bound_target(
        &self,
        id: &SemanticId,
        action: &str,
    ) -> Option<crate::bound_dispatch::BoundActionTarget> {
        self.bound_actions
            .get(&(id.clone(), action.to_owned()))
            .cloned()
    }
    fn refresh_impl<const BOUND: bool>(
        &mut self,
        root: &dyn Widget,
        node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
        admitted: Option<&AdmittedNodeNames>,
        focused: Option<TaffyNodeId>,
        resources: &[Resource],
        components: &[crate::bound_dispatch::BoundResourceSnapshot],
        doc: Option<&tilo_actions::Doc>,
    ) -> TreeDelta {
        let ptr_to_taffy = WidgetNodeMap::new::<BOUND>(root, node_map, admitted);

        let identities = crate::semantic_identity::IdentityScope::new::<BOUND>(&[root]);
        let mut builder = Build::<BOUND> {
            nodes: HashMap::new(),
            order: Vec::new(),
            prompted: HashMap::new(),
            bound_actions: HashMap::new(),
            duplicate_ids: Vec::new(),
            overlays: Vec::new(),
            ptr_to_taffy: &ptr_to_taffy,
            focused,
            identities,
            doc,
            resource_refs: HashMap::new(),
            declared_resources: Vec::new(),
        };

        let root_id = SemanticId::root();
        builder.order.push(root_id.clone());

        let mut root_children: Vec<SemanticId> = Vec::new();
        builder.visit_widget(
            root,
            &root_id,
            &mut root_children,
            "",
            &mut HashMap::new(),
            None,
        );

        // Resource affordances hang off the root as their own subtrees.
        for res in resources {
            let child = builder.visit_resource(res, None);
            root_children.push(child);
        }

        for component in components {
            for resource in &component.resources {
                root_children.push(builder.visit_resource(resource, Some(&component.binding)));
            }
        }

        builder.nodes.insert(
            root_id.clone(),
            SemanticNode::window_root(root_children, self.surface),
        );
        builder.resolve_references();

        let delta = self.diff(&builder.nodes);

        self.nodes = builder.nodes;
        self.order = builder.order;
        self.root = root_id;
        self.prompted = builder.prompted;
        self.bound_actions = builder.bound_actions;
        self.duplicate_ids = builder.duplicate_ids;
        self.overlays = builder.overlays;
        if !delta.is_empty() {
            self.revision = self.revision.saturating_add(1);
        }
        delta
    }

    fn diff(&self, next: &HashMap<SemanticId, SemanticNode>) -> TreeDelta {
        let mut delta = TreeDelta::default();
        for (id, node) in next {
            match self.nodes.get(id) {
                None => delta.added.push(id.clone()),
                Some(prev) if !prev.same_exported_content(node) => delta.updated.push(id.clone()),
                Some(_) => {}
            }
        }
        for id in self.nodes.keys() {
            if !next.contains_key(id) {
                delta.removed.push(id.clone());
            }
        }
        delta.added.sort();
        delta.removed.sort();
        delta.updated.sort();
        delta
    }

    pub fn root(&self) -> &SemanticId {
        &self.root
    }

    /// How many refreshes have changed the tree. Every consumer that refreshes
    /// (AccessKit, the action surface, inspection) advances the same count,
    /// so a watcher compares revisions rather than depending on seeing each
    /// delta itself.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The overlays open as of the last refresh, in reading order. An
    /// overlay is named by the node of the widget that owns it; a
    /// presentational widget (no node of its own) cannot own one.
    pub fn open_overlays(&self) -> &[crate::view::OpenOverlay] {
        &self.overlays
    }

    pub fn node(&self, id: &SemanticId) -> Option<&SemanticNode> {
        self.nodes.get(id)
    }

    /// Includes the root.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree holds only the root.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// Stable DFS reading order.
    pub fn iter(&self) -> impl Iterator<Item = &SemanticNode> {
        self.order.iter().filter_map(move |id| self.nodes.get(id))
    }

    /// Duplicate durable keys are never silently accepted as stable identity.
    pub fn duplicate_ids(&self) -> &[SemanticId] {
        &self.duplicate_ids
    }

    /// The first node whose accessible [`name`](SemanticNode::name) equals
    /// `name`, in reading order — the ergonomic handle for a CLI / agent that
    /// addresses elements by label ("focus the node called 'Volume'").
    pub fn find_by_name(&self, name: &str) -> Option<&SemanticNode> {
        self.iter().find(|n| n.name.as_deref() == Some(name))
    }

    /// Every live semantic path to a control binding id, shortest first.
    /// Multiple buttons may intentionally bind the same domain action; this
    /// gives agents and presence projection a deterministic way to prefer the
    /// nearest currently rendered affordance without replacing direct action
    /// invocation with simulated clicks.
    pub fn control_paths(&self, control_id: &str) -> Vec<Vec<SemanticId>> {
        if control_id.is_empty() {
            return Vec::new();
        }
        let mut parents = HashMap::<SemanticId, SemanticId>::new();
        for node in self.nodes.values() {
            for child in &node.children {
                parents.insert(child.clone(), node.id.clone());
            }
        }
        let mut paths = self
            .iter()
            .filter(|node| node.durable_identity && node.control_key.as_deref() == Some(control_id))
            .filter_map(|node| {
                let mut path = vec![node.id.clone()];
                let mut cursor = node.id.clone();
                while cursor != self.root {
                    cursor = parents.get(&cursor)?.clone();
                    path.push(cursor.clone());
                }
                path.reverse();
                Some(path)
            })
            .collect::<Vec<_>>();
        paths.sort_by(|left, right| {
            left.len()
                .cmp(&right.len())
                .then_with(|| left.last().cmp(&right.last()))
        });
        paths
    }

    pub fn action(&self, id: &SemanticId, name: &str) -> Option<&SemanticAction> {
        self.nodes.get(id).and_then(|n| n.action(name))
    }

    /// Project every currently operable accessibility node into the canonical
    /// Tilo resource/action surface. Bespoke app actions remain preferable for
    /// domain operations; this projection closes the driveability gap for
    /// ordinary controls such as buttons, fields, tabs, toggles, and sliders.
    pub(crate) fn action_resources(&self) -> Vec<Resource> {
        self.iter()
            .filter(|node| node.id != self.root && !node.id.0.starts_with("r:"))
            .filter(|node| node.durable_identity)
            .filter(|node| !self.duplicate_ids.contains(&node.id))
            .filter_map(|node| {
                let verbs = node
                    .actions
                    .iter()
                    .filter_map(|action| match &action.kind {
                        SemanticActionKind::Verb(verb)
                            if semantic_operation_kind(node.role, verb) == OperationKind::View
                                || node.semantic_target.is_some() =>
                        {
                            Some((action, verb))
                        }
                        SemanticActionKind::Verb(_)
                        | SemanticActionKind::App(_)
                        | SemanticActionKind::AppPrompted { .. }
                        | SemanticActionKind::AppDeferred { .. } => None,
                    })
                    .collect::<Vec<_>>();
                if verbs.is_empty() {
                    return None;
                }

                let label = node
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", node.role));
                // A semantic id may contain KEY_SEP when a control uses an
                // encoded resource/action address as its stable key.  Keep
                // the exact id in the operation payload for dispatch, but
                // encode it before embedding it in a ResourceId: KEY_SEP is
                // the action-address delimiter and is forbidden there.
                let encoded_node_id = percent_encoding::utf8_percent_encode(
                    node.id.as_str(),
                    percent_encoding::NON_ALPHANUMERIC,
                );
                let mut resource =
                    Resource::new(format!("ui:{encoded_node_id}"), "tilo.ui.control", label);
                for (action, verb) in verbs {
                    let operation = SemanticOperation {
                        node: node.id.0.clone(),
                        action: action.name.clone(),
                        value: None,
                    };
                    let spec = match verb {
                        Action::SetValue(_) => ActionSpec::prompted_operation(
                            action.name.clone(),
                            action.label.clone(),
                            "Value",
                            node.value.as_deref().unwrap_or(""),
                            semantic_operation_kind(node.role, verb),
                            SEMANTIC_OPERATION_TYPE,
                            move |value| {
                                let operation = SemanticOperation {
                                    node: operation.node.clone(),
                                    action: operation.action.clone(),
                                    value: Some(value.to_string()),
                                };
                                match EncodedAction::encode_operation(
                                    SEMANTIC_OPERATION_TYPE,
                                    &operation,
                                ) {
                                    Ok(action) => action,
                                    Err(error) => {
                                        tracing::error!(
                                            %error,
                                            "semantic operation could not be serialized"
                                        );
                                        EncodedAction {
                                            type_id: SEMANTIC_OPERATION_TYPE.to_string(),
                                            payload: Vec::new(),
                                        }
                                    }
                                }
                            },
                        ),
                        _ => match ActionSpec::operation(
                            action.name.clone(),
                            action.label.clone(),
                            semantic_operation_kind(node.role, verb),
                            SEMANTIC_OPERATION_TYPE,
                            &operation,
                        ) {
                            Ok(spec) => spec,
                            Err(_) => continue,
                        },
                    };
                    let spec = if matches!(verb, Action::SetValue(_)) {
                        continuation::annotate(spec, node, action)
                    } else {
                        spec
                    };
                    resource.actions.push(spec);
                }
                Some(resource)
            })
            .collect()
    }

    /// `None` if `(id, name)` is not a prompted action.
    pub fn build_prompted(&self, id: &SemanticId, name: &str, arg: &str) -> Option<EncodedAction> {
        self.prompted
            .get(&(id.clone(), name.to_string()))
            .map(|build| build(arg))
    }
}

/// Mirrors the implicit/role actions
/// [`accesskit_bridge`](crate::accesskit_bridge) emits, so the retained
/// tree and the AT-SPI export agree on what a widget can do.
fn verbs_for(access: &AccessNode) -> Vec<SemanticAction> {
    let mut out = Vec::new();
    let s = &access.state;
    if s.focusable && !s.disabled {
        out.push(SemanticAction {
            name: "focus".into(),
            label: "Focus".into(),
            kind: SemanticActionKind::Verb(Action::Focus),
        });
    }
    if s.context_menu && !s.disabled {
        out.push(SemanticAction {
            name: "show-context-menu".into(),
            label: "Show context menu".into(),
            kind: SemanticActionKind::Verb(Action::ShowContextMenu),
        });
    }
    if !s.disabled
        && matches!(
            access.role,
            Role::Button
                | Role::ToggleButton
                | Role::Checkbox
                | Role::RadioButton
                | Role::Switch
                | Role::Tab
                | Role::MenuItem
                | Role::ListItem
                | Role::TreeItem
                | Role::Link
        )
    {
        out.push(SemanticAction {
            name: "activate".into(),
            label: "Activate".into(),
            kind: SemanticActionKind::Verb(Action::Activate),
        });
    }
    match access.role {
        Role::Slider | Role::SpinButton if !s.disabled => {
            out.push(SemanticAction {
                name: "increment".into(),
                label: "Increment".into(),
                kind: SemanticActionKind::Verb(Action::Increment),
            });
            out.push(SemanticAction {
                name: "decrement".into(),
                label: "Decrement".into(),
                kind: SemanticActionKind::Verb(Action::Decrement),
            });
        }
        Role::ComboBox | Role::TreeItem => {
            if let Some(expanded) = s.expanded {
                if expanded {
                    out.push(SemanticAction {
                        name: "collapse".into(),
                        label: "Collapse".into(),
                        kind: SemanticActionKind::Verb(Action::Collapse),
                    });
                } else {
                    out.push(SemanticAction {
                        name: "expand".into(),
                        label: "Expand".into(),
                        kind: SemanticActionKind::Verb(Action::Expand),
                    });
                }
            }
            if matches!(access.role, Role::ComboBox) && !s.read_only && !s.disabled {
                out.push(SemanticAction {
                    name: "set-value".into(),
                    label: "Set value".into(),
                    kind: SemanticActionKind::Verb(Action::SetValue(String::new())),
                });
            }
        }
        Role::TextInput | Role::MultilineTextInput if !s.read_only && !s.disabled => {
            out.push(SemanticAction {
                name: "set-value".into(),
                label: "Set value".into(),
                kind: SemanticActionKind::Verb(Action::SetValue(String::new())),
            });
        }
        _ => {}
    }
    out
}

fn semantic_operation_kind(role: Role, action: &Action) -> OperationKind {
    match action {
        Action::Focus
        | Action::Expand
        | Action::Collapse
        | Action::ScrollIntoView
        | Action::SetSelection { .. }
        | Action::ShowContextMenu => OperationKind::View,
        Action::Activate
            if matches!(
                role,
                Role::Tab | Role::Link | Role::ListItem | Role::TreeItem | Role::MenuItem
            ) =>
        {
            OperationKind::View
        }
        Action::Activate
        | Action::ToggleCheck
        | Action::Increment
        | Action::Decrement
        | Action::SetValue(_) => OperationKind::Effect,
    }
}

fn intersect_clip(clip: Option<Rect>, child_clip: Option<Rect>) -> Option<Rect> {
    match (clip, child_clip) {
        (Some(outer), Some(inner)) => Some(outer.intersect(inner)),
        (outer, inner) => outer.or(inner),
    }
}

/// Separate from [`SemanticTree`] so the retained frame is only replaced
/// once the new one is fully built (and diffed against).
struct Build<'a, const BOUND: bool> {
    nodes: HashMap<SemanticId, SemanticNode>,
    order: Vec<SemanticId>,
    prompted: PromptedMap,
    bound_actions: HashMap<(SemanticId, String), crate::bound_dispatch::BoundActionTarget>,
    ptr_to_taffy: &'a WidgetNodeMap<'a>,
    focused: Option<TaffyNodeId>,
    duplicate_ids: Vec<SemanticId>,
    overlays: Vec<crate::view::OpenOverlay>,
    /// The shared identity derivation for this walk.
    identities: crate::semantic_identity::IdentityScope,
    /// The document a widget's [`Referent`](tilo_reference::Referent) reads
    /// to say what its content is about. `None` for a bare structural walk.
    doc: Option<&'a tilo_actions::Doc>,
    /// Every published resource by id, so a widget that declares the resource
    /// it presents resolves to the same address the resource node carries.
    resource_refs: HashMap<String, (tilo_reference::ResourceRef, Sensitivity)>,
    /// Nodes that declared the resource they present, resolved once the
    /// whole forest is known.
    declared_resources: Vec<(SemanticId, String)>,
}

impl<const BOUND: bool> Build<'_, BOUND> {
    fn insert_node(&mut self, node: SemanticNode) {
        let id = node.id.clone();
        if self.nodes.insert(id.clone(), node).is_some() {
            self.duplicate_ids.push(id);
        }
    }

    /// Resolve declared widget resources once the whole forest is known.
    fn resolve_references(&mut self) {
        for (id, resource) in std::mem::take(&mut self.declared_resources) {
            if let Some(node) = self.nodes.get_mut(&id) {
                let resolved = self.resource_refs.get(&resource);
                node.reference = resolved.map(|(reference, _)| reference.clone());
                // A row presenting a person's contact is as sensitive as the
                // contact itself, whatever the row declared.
                if let Some((_, sensitivity)) = resolved {
                    node.sensitivity = node.sensitivity.max(*sensitivity);
                }
                if node.sensitivity.is_secret() {
                    node.value = None;
                }
            }
        }
    }

    fn declare_resource(&mut self, id: &SemanticId, access: &AccessNode) {
        if let Some(resource) = &access.resource {
            self.declared_resources.push((id.clone(), resource.clone()));
        }
    }

    /// A presentational widget (`accessibility() == None`) is collapsed
    /// out and its children reparented — matching the AccessKit bridge.
    /// `path_base` and `sibling_counts` place the widget in the widget tree
    /// for its structural id; `clip` is every ancestor's child clip so far.
    #[allow(
        clippy::too_many_lines,
        reason = "one widget's identity, geometry and semantics are derived together"
    )]
    fn visit_widget(
        &mut self,
        w: &dyn Widget,
        semantic_parent: &SemanticId,
        parent_children: &mut Vec<SemanticId>,
        path_base: &str,
        sibling_counts: &mut HashMap<String, usize>,
        clip: Option<Rect>,
    ) {
        let Ok(_entry) = crate::bound_dispatch::enter_traversal::<BOUND>(w) else {
            return;
        };
        let taffy_id = self.ptr_to_taffy.get(w);
        let path = crate::semantic_identity::structural_path(w, path_base, sibling_counts);
        let child_clip = intersect_clip(clip, w.child_clip_rect());
        let mut child_counts = HashMap::new();
        match self.identities.access_of(w) {
            Some(access) => {
                let crate::semantic_identity::Identified {
                    id,
                    durable,
                    children_base,
                    shared_key,
                } = self.identities.enter(w, &access, semantic_parent, path);
                // Reported once per frame, never used as identity: two
                // siblings under one key would each answer to it.
                if let Some(shared) = shared_key
                    && !self.duplicate_ids.contains(&shared)
                {
                    self.duplicate_ids.push(shared);
                }
                for state in w.open_overlays() {
                    self.overlays.push(crate::view::OpenOverlay {
                        owner: id.as_str().to_string(),
                        state,
                    });
                }

                let mut children: Vec<SemanticId> = Vec::new();
                let mut hosted = HostedByIndex::new();
                for (child_index, child) in w.children().into_iter().enumerate() {
                    if let Some(mut path) = w.accessibility_child_path(child_index)
                        && !path.is_empty()
                    {
                        let parent_index = path.remove(0);
                        hosted.entry(parent_index).or_default().push((path, child));
                    } else {
                        self.visit_widget(
                            child,
                            &id,
                            &mut children,
                            &children_base,
                            &mut child_counts,
                            child_clip,
                        );
                    }
                }
                for (i, synth) in access.children.iter().enumerate() {
                    let child = self.visit_synthetic(
                        &id,
                        durable,
                        i,
                        synth,
                        taffy_id,
                        hosted.remove(&i).unwrap_or_default(),
                        child_clip,
                    );
                    children.push(child);
                }

                self.identities.leave();
                let mut state = access.state;
                if let (Some(t), Some(f)) = (taffy_id, self.focused)
                    && t == f
                {
                    state.focused = true;
                }

                // Disabled/read-only gate the verbs but focus does not, so
                // compute against the focus-updated state.
                let actions = verbs_for(&AccessNode {
                    state,
                    ..access.clone()
                });

                let node = SemanticNode {
                    id: id.clone(),
                    durable_identity: durable,
                    role: access.role,
                    name: access.name.clone(),
                    value: if access.sensitivity.is_secret() {
                        None
                    } else {
                        access.value.clone()
                    },
                    sensitivity: access.sensitivity,
                    state,
                    bounds: Some(w.get_rect()),
                    clip,
                    actions,
                    children,
                    widget_taffy_id: taffy_id.map(u64::from),
                    semantic_target: access.action_target.clone(),
                    control_key: access.semantic_key.clone(),
                    reference: if access.resource.is_some() {
                        None
                    } else {
                        self.doc.and_then(|doc| {
                            w.referent()
                                .and_then(|referent| referent.selection_reference(doc))
                        })
                    },
                    transient: access.transient,
                };
                self.declare_resource(&id, &access);
                self.order.push(id.clone());
                self.insert_node(node);
                parent_children.push(id);
            }
            None => {
                for child in w.children() {
                    self.visit_widget(
                        child,
                        semantic_parent,
                        parent_children,
                        &path,
                        &mut child_counts,
                        child_clip,
                    );
                }
            }
        }
    }

    /// A virtual row/cell — has no backing widget, but may carry geometry.
    fn visit_synthetic(
        &mut self,
        parent: &SemanticId,
        parent_durable: bool,
        index: usize,
        access: &AccessNode,
        owner: Option<TaffyNodeId>,
        hosted: Vec<HostedWidget<'_>>,
        clip: Option<Rect>,
    ) -> SemanticId {
        let id = crate::semantic_identity::synthetic_id(parent, index, access);
        let durable = access.key.is_some() && parent_durable;
        let semantic_target = access.action_target.as_deref();
        let mut children: Vec<SemanticId> = Vec::new();
        let direct = hosted
            .iter()
            .filter(|(path, _)| path.is_empty())
            .map(|(_, child)| *child)
            .collect::<Vec<_>>();
        if direct.is_empty() {
            let mut nested = HostedByIndex::new();
            for (mut path, child) in hosted {
                let child_index = path.remove(0);
                nested.entry(child_index).or_default().push((path, child));
            }
            for (i, child) in access.children.iter().enumerate() {
                let c = self.visit_synthetic(
                    &id,
                    durable,
                    i,
                    child,
                    owner,
                    nested.remove(&i).unwrap_or_default(),
                    clip,
                );
                children.push(c);
            }
        } else {
            let mut counts = HashMap::new();
            for child in direct {
                self.visit_widget(child, &id, &mut children, &id.0, &mut counts, clip);
            }
        }
        let node = SemanticNode {
            id: id.clone(),
            durable_identity: durable,
            role: access.role,
            name: access.name.clone(),
            value: if access.sensitivity.is_secret() {
                None
            } else {
                access.value.clone()
            },
            sensitivity: access.sensitivity,
            state: access.state,
            bounds: access.bounds.map(crate::a11y::AccessBounds::to_rect),
            clip,
            actions: if semantic_target.is_some() {
                verbs_for(access)
            } else {
                Vec::new()
            },
            children,
            widget_taffy_id: semantic_target.and(owner).map(u64::from),
            semantic_target: semantic_target.map(str::to_owned),
            control_key: None,
            reference: None,
            transient: access.transient,
        };
        self.declare_resource(&id, access);
        self.order.push(id.clone());
        self.insert_node(node);
        id
    }

    fn visit_resource(
        &mut self,
        res: &Resource,
        binding: Option<&crate::bound_dispatch::CallbackBinding>,
    ) -> SemanticId {
        let id = binding.map_or_else(
            || SemanticId::resource(res.id.as_str()),
            |binding| {
                SemanticId(format!(
                    "ri:{}:{}:{}",
                    binding.instance_id,
                    binding.lifetime_id,
                    res.id.as_str()
                ))
            },
        );

        let mut actions: Vec<SemanticAction> = Vec::new();
        for spec in &res.actions {
            actions.push(self.resource_action(&id, res.id.as_str(), spec));
            if let Some(binding) = binding {
                self.bound_actions.insert(
                    (id.clone(), spec.id.clone()),
                    crate::bound_dispatch::BoundActionTarget {
                        binding: binding.clone(),
                        address: crate::bound_dispatch::BoundActionAddress {
                            instance_id: binding.instance_id,
                            address: tilo_actions::ActionAddress::new(res.id.as_str(), &spec.id),
                        },
                    },
                );
            }
        }

        let mut children: Vec<SemanticId> = Vec::new();
        for child in &res.children {
            children.push(self.visit_resource(child, binding));
        }

        let reference = tilo_reference::ResourceRef::whole(&res.kind, res.id.as_str(), &res.label);
        self.resource_refs
            .entry(res.id.as_str().to_owned())
            .or_insert_with(|| (reference.clone(), res.sensitivity));
        let node = SemanticNode {
            id: id.clone(),
            // Component semantic addresses remain valid only for this captured lifetime.
            durable_identity: binding.is_none(),
            role: Role::Group,
            name: Some(res.label.clone()),
            value: None,
            sensitivity: res.sensitivity,
            state: AccessState::default(),
            bounds: None,
            clip: None,
            actions,
            children,
            widget_taffy_id: None,
            semantic_target: None,
            control_key: None,
            reference: Some(reference),
            transient: false,
        };
        self.order.push(id.clone());
        self.insert_node(node);
        id
    }

    fn resource_action(
        &mut self,
        node_id: &SemanticId,
        resource: &str,
        spec: &ActionSpec,
    ) -> SemanticAction {
        let kind = match &spec.input {
            ActionInput::Immediate(enc) => SemanticActionKind::App(enc.clone()),
            ActionInput::Deferred { .. } => SemanticActionKind::AppDeferred {
                address: tilo_actions::ActionAddress::new(resource, &spec.id),
            },
            ActionInput::Prompted {
                prompt,
                placeholder,
                collects,
                build,
            } => {
                self.prompted
                    .insert((node_id.clone(), spec.id.clone()), build.clone());
                SemanticActionKind::AppPrompted {
                    prompt: prompt.clone(),
                    placeholder: placeholder.clone(),
                    collects: *collects,
                }
            }
        };
        SemanticAction {
            name: spec.id.clone(),
            label: spec.label.clone(),
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use crate::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
    use crate::widget::{Button, Column, TextInput, Widget};
    use std::collections::HashMap;
    use taffy::TaffyTree;

    /// Lays a widget tree out the way the App does, populating the
    /// `node_map` the tree keys ids off.
    fn layout(root: &mut dyn Widget) -> HashMap<taffy::NodeId, *mut dyn Widget> {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let node = root.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
            )
            .unwrap();
        root.apply_layout(&taffy, node, vello::kurbo::Point::ZERO);
        node_map
    }

    #[test]
    fn semantic_ids_are_durable_and_exported_only_when_stable() {
        // Case `identity_is_stable_across_frames`.
        {
            let mut col = Column::new()
                .push(Box::new(Button::new("a", "Alpha", None)))
                .push(Box::new(Button::new("b", "Beta", None)));
            let node_map = layout(&mut col);

            let mut tree = SemanticTree::new();
            let first = tree.refresh(&col, &node_map, None, &[]);
            // The root pre-exists from `new()`, so it registers as updated,
            // not added, once it gains children.
            assert!(first.removed.is_empty());
            assert!(
                first.added.len() >= 2,
                "the two buttons are added, got {:?}",
                first.added
            );
            let ids_first: Vec<SemanticId> = tree.iter().map(|n| n.id.clone()).collect();
            assert!(
                ids_first.len() >= 3,
                "root + two buttons, got {ids_first:?}"
            );
            assert!(
                ids_first.iter().any(|id| id.as_str() == "t:a")
                    && ids_first.iter().any(|id| id.as_str() == "t:b"),
                "buttons are identified by their unique test ids, got {ids_first:?}"
            );

            let second = tree.refresh(&col, &node_map, None, &[]);
            assert!(
                second.is_empty(),
                "a stable frame yields no delta, got {second:?}"
            );
            let ids_second: Vec<SemanticId> = tree.iter().map(|n| n.id.clone()).collect();
            assert_eq!(
                ids_first, ids_second,
                "node identity + order survive a refresh"
            );
        }

        // Case `duplicate_control_locations_choose_the_shortest_semantic_path`.
        {
            let mut tree = SemanticTree::new();
            let group = SemanticId("root~k:advanced".into());
            let deep = SemanticId("root~k:advanced~k:save".into());
            let shallow = SemanticId("root~k:save".into());
            let node = |id: SemanticId, children: Vec<SemanticId>| SemanticNode {
                control_key: id.0.rsplit("~k:").next().map(str::to_owned),
                id,
                durable_identity: true,
                role: Role::Button,
                name: Some("Save".into()),
                value: None,
                sensitivity: Sensitivity::Public,
                state: AccessState::default(),
                bounds: Some(Rect::new(0.0, 0.0, 10.0, 10.0)),
                clip: None,
                actions: Vec::new(),
                children,
                widget_taffy_id: None,
                semantic_target: None,
                reference: None,
                transient: false,
            };
            tree.nodes
                .insert(group.clone(), node(group.clone(), vec![deep.clone()]));
            tree.nodes
                .insert(deep.clone(), node(deep.clone(), Vec::new()));
            tree.nodes
                .insert(shallow.clone(), node(shallow.clone(), Vec::new()));
            tree.nodes.get_mut(&tree.root).unwrap().children = vec![group.clone(), shallow.clone()];
            tree.order = vec![tree.root.clone(), group, deep.clone(), shallow.clone()];

            let routes = tree.control_paths("save");
            assert_eq!(routes.len(), 2);
            assert_eq!(routes[0], vec![tree.root.clone(), shallow]);
            assert_eq!(routes[1].last(), Some(&deep));
        }

        // Case `durable_widget_keys_survive_layout_rebuild_and_sibling_reorder`.
        {
            let mut first = Column::new()
                .push(Box::new(Button::new("alpha", "Alpha", None)))
                .push(Box::new(Button::new("beta", "Beta", None)));
            let first_map = layout(&mut first);
            let mut tree = SemanticTree::new();
            tree.refresh(&first, &first_map, None, &[]);
            let alpha = tree.find_by_name("Alpha").expect("alpha").id.clone();
            let beta = tree.find_by_name("Beta").expect("beta").id.clone();
            let alpha_before = tree.node(&alpha).expect("alpha").clone();
            let mut rebound = alpha_before.clone();
            rebound.widget_taffy_id = Some(
                alpha_before
                    .widget_taffy_id
                    .expect("laid-out alpha")
                    .saturating_add(1),
            );
            let mut rebound_frame = tree.nodes.clone();
            rebound_frame.insert(alpha.clone(), rebound);
            assert!(
                tree.diff(&rebound_frame).is_empty(),
                "a frame-local routing address is not exported semantic content"
            );
            assert_eq!(alpha.as_str(), "t:alpha");

            let mut rebuilt = Column::new()
                .push(Box::new(Button::new("unrelated", "Unrelated", None)))
                .push(Box::new(Button::new("beta", "Beta", None)))
                .push(Box::new(Button::new("alpha", "Alpha", None)));
            let rebuilt_map = layout(&mut rebuilt);
            tree.refresh(&rebuilt, &rebuilt_map, None, &[]);
            assert_eq!(tree.find_by_name("Alpha").expect("alpha").id, alpha);
            assert_eq!(tree.find_by_name("Beta").expect("beta").id, beta);
            assert_ne!(
                tree.node(&alpha).expect("alpha").widget_taffy_id,
                alpha_before.widget_taffy_id,
                "the rebuilt widget is routed through its new layout node"
            );
            assert!(tree.duplicate_ids().is_empty());
        }

        // Case `encoded_action_keys_remain_valid_resource_ids`.
        {
            let mut button = Button::new("settings.section:network\u{1f}open", "Network", None);
            let node_map = layout(&mut button);
            let mut tree = SemanticTree::new();
            tree.refresh(&button, &node_map, None, &[]);

            let resources = tree.action_resources();
            assert!(
                resources
                    .iter()
                    .all(|resource| { !resource.id.as_str().contains(tilo_actions::KEY_SEP) })
            );
            assert!(
                tilo_actions::validate_surface_contracts(
                    &resources,
                    &tilo_actions::ActionRegistry::new(),
                )
                .is_empty()
            );
        }

        // Case `frame_local_control_identity_is_not_exported`.
        {
            let mut button = Button::new("go", "Go", None);
            let node_map = layout(&mut button);
            let mut tree = SemanticTree::new();
            tree.refresh(&button, &node_map, None, &[]);
            let id = tree
                .iter()
                .find(|node| node.name.as_deref() == Some("Go"))
                .expect("button node")
                .id
                .clone();
            tree.nodes.get_mut(&id).unwrap().durable_identity = false;

            assert!(tree.action_resources().is_empty());
        }
    }

    /// A labelled group with no key of its own, so the widgets below it are
    /// addressed structurally.
    struct Panel {
        label: &'static str,
        children: Vec<Box<dyn Widget>>,
    }

    impl Widget for Panel {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let nodes: Vec<taffy::NodeId> = self
                .children
                .iter_mut()
                .map(|child| child.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy
                .new_with_children(taffy::Style::default(), &nodes)
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(
            &mut self,
            taffy: &TaffyTree<()>,
            node: taffy::NodeId,
            origin: vello::kurbo::Point,
        ) {
            let children = taffy.children(node).unwrap_or_default();
            for (child, node) in self.children.iter_mut().zip(children) {
                child.apply_layout(taffy, node, origin);
            }
        }
        fn draw(
            &mut self,
            _: &mut vello::Scene,
            _: vello::kurbo::Affine,
            _: &mut crate::text_render::TextRenderer,
            _: &Theme,
            _: &crate::presence::PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _: &winit::event::WindowEvent,
            _: &mut crate::widget::EventCtx<'_>,
        ) {
        }
        fn name(&self) -> &str {
            "Panel"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        fn accessibility(&self) -> Option<AccessNode> {
            Some(AccessNode::new(Role::Group).with_name(self.label))
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.children.iter().map(AsRef::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.children
                .iter_mut()
                .map(|child| &mut **child as &mut (dyn Widget + '_))
                .collect()
        }
    }

    fn fields() -> Box<dyn Widget> {
        Box::new(
            Column::new()
                .push(Box::new(
                    TextInput::new(Vec::new()).with_accessible_name("First"),
                ))
                .push(Box::new(
                    TextInput::new(Vec::new()).with_accessible_name("Second"),
                )),
        )
    }

    fn ids(tree: &SemanticTree) -> Vec<SemanticId> {
        tree.iter().map(|node| node.id.clone()).collect()
    }

    #[test]
    fn ids_survive_a_structural_rebuild_and_never_name_a_layout_node() {
        let build = || Panel {
            label: "Form",
            children: vec![fields()],
        };
        let mut first = build();
        let first_map = layout(&mut first);
        let mut tree = SemanticTree::new();
        tree.refresh(&first, &first_map, None, &[]);
        let before = ids(&tree);

        // A fresh widget tree in a fresh layout arena: every Taffy node id
        // is reassigned, and none of the semantic ids may move with them.
        let mut taffy = TaffyTree::<()>::new();
        for _ in 0..17 {
            let _ = taffy.new_leaf(taffy::Style::default());
        }
        let mut rebuilt = build();
        let mut rebuilt_map = HashMap::new();
        let node = rebuilt.build_layout(&mut taffy, &Theme::dark(), &mut rebuilt_map);
        rebuilt.apply_layout(&taffy, node, vello::kurbo::Point::ZERO);
        assert!(
            rebuilt_map.keys().all(|key| !first_map.contains_key(key)),
            "the rebuild really used different layout nodes"
        );
        let delta = tree.refresh(&rebuilt, &rebuilt_map, None, &[]);
        assert_eq!(ids(&tree), before, "identity survives the rebuild");
        assert!(
            delta.added.is_empty() && delta.removed.is_empty(),
            "{delta:?}"
        );
        assert!(
            before
                .iter()
                .all(|id| !id.as_str().starts_with('w') || id.as_str().contains('[')),
            "no layout-node id is part of identity: {before:?}"
        );
        let second = tree.find_by_name("Second").unwrap();
        assert_eq!(second.id.as_str(), "Panel[0]/Column[0]/TextInput[1]");
        assert!(!second.durable_identity, "a positional path is not durable");
    }

    #[test]
    fn structural_ids_are_unique_among_same_named_siblings_and_match_headless_paths() {
        let mut panel = Panel {
            label: "Form",
            children: vec![fields(), fields()],
        };
        let map = layout(&mut panel);
        let mut tree = SemanticTree::new();
        tree.refresh(&panel, &map, None, &[]);
        let inputs: Vec<&str> = tree
            .iter()
            .filter(|node| node.role == Role::TextInput)
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(
            inputs,
            [
                "Panel[0]/Column[0]/TextInput[0]",
                "Panel[0]/Column[0]/TextInput[1]",
                "Panel[0]/Column[1]/TextInput[0]",
                "Panel[0]/Column[1]/TextInput[1]",
            ]
        );
        assert_eq!(
            tree.duplicate_ids(),
            [SemanticId("Panel[0]~k:text_input".into())],
            "the fields' shared fallback key is reported and not used as identity"
        );
        let driver = crate::headless::Driver::new(
            crate::State::new(),
            Box::new(Panel {
                label: "Form",
                children: vec![fields(), fields()],
            }),
            (800, 600),
        );
        let targets = driver.targets();
        for id in inputs {
            assert!(
                targets.contains_key(id),
                "semantic id {id} is also a headless target"
            );
        }
    }

    #[test]
    fn a_shared_test_id_identifies_none_of_its_widgets() {
        let mut col = Column::new()
            .push(Box::new(Button::new("save", "Save draft", None)))
            .push(Box::new(Button::new("save", "Save copy", None)));
        let map = layout(&mut col);
        let mut tree = SemanticTree::new();
        tree.refresh(&col, &map, None, &[]);
        let draft = tree.find_by_name("Save draft").unwrap();
        let copy = tree.find_by_name("Save copy").unwrap();
        assert_ne!(draft.id, copy.id, "both widgets stay addressable");
        assert_eq!(draft.id.as_str(), "Column[0]/Button[0]");
        assert_eq!(copy.id.as_str(), "Column[0]/Button[1]");
        assert!(!draft.durable_identity && !copy.durable_identity);
        assert_eq!(
            tree.duplicate_ids(),
            [SemanticId("root~k:save".into())],
            "the shared control key is reported, not silently accepted"
        );
    }

    #[test]
    fn a_widget_that_presents_a_resource_carries_its_reference_and_sensitivity() {
        struct Row;
        impl Widget for Row {
            fn build_layout(
                &mut self,
                taffy: &mut TaffyTree<()>,
                _: &Theme,
                node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
            ) -> taffy::NodeId {
                let node = taffy.new_leaf(taffy::Style::default()).unwrap();
                node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
                node
            }
            fn apply_layout(
                &mut self,
                _: &TaffyTree<()>,
                _: taffy::NodeId,
                _: vello::kurbo::Point,
            ) {
            }
            fn draw(
                &mut self,
                _: &mut vello::Scene,
                _: vello::kurbo::Affine,
                _: &mut crate::text_render::TextRenderer,
                _: &Theme,
                _: &crate::presence::PresenceCtx,
            ) {
            }
            fn handle_event(
                &mut self,
                _: &winit::event::WindowEvent,
                _: &mut crate::widget::EventCtx<'_>,
            ) {
            }
            fn name(&self) -> &str {
                "Row"
            }
            fn get_rect(&self) -> Rect {
                Rect::new(0.0, 0.0, 10.0, 10.0)
            }
            fn accessibility(&self) -> Option<AccessNode> {
                Some(
                    AccessNode::new(Role::ListItem)
                        .with_name("Ada Lovelace")
                        .with_resource("contacts.person:1"),
                )
            }
        }
        let resource = Resource::new("contacts.person:1", "contacts.person", "Ada Lovelace")
            .with_sensitivity(Sensitivity::Personal);
        let mut row = Row;
        let map = layout(&mut row);
        let mut tree = SemanticTree::new();
        tree.refresh(&row, &map, None, std::slice::from_ref(&resource));
        let node = tree
            .iter()
            .find(|node| node.role == Role::ListItem)
            .expect("row node");
        let reference = node.reference.as_ref().expect("resolved reference");
        assert_eq!(reference.kind, "contacts.person");
        assert_eq!(reference.id, "contacts.person:1");
        assert_eq!(
            node.sensitivity,
            Sensitivity::Personal,
            "a row presenting a person is as sensitive as the person"
        );
        let resource_node = tree
            .node(&SemanticId("r:contacts.person:1".into()))
            .expect("resource node");
        assert_eq!(resource_node.reference.as_ref(), Some(reference));
    }

    #[test]
    fn nodes_afford_only_the_actions_they_declare() {
        // Case `semantic_projection_classifies_navigation_as_view`.
        {
            assert_eq!(
                semantic_operation_kind(Role::Button, &Action::Focus),
                OperationKind::View
            );
            assert_eq!(
                semantic_operation_kind(Role::Tab, &Action::Activate),
                OperationKind::View
            );
            assert_eq!(
                semantic_operation_kind(Role::Button, &Action::Activate),
                OperationKind::Effect
            );
        }

        // Case `button_node_affords_focus_and_activate_verbs`.
        {
            let mut btn = Button::new("go", "Go", None);
            let node_map = layout(&mut btn);
            let mut tree = SemanticTree::new();
            tree.refresh(&btn, &node_map, None, &[]);
            let node = tree
                .iter()
                .find(|n| n.name.as_deref() == Some("Go"))
                .expect("button node");
            assert!(
                node.action("activate").is_some(),
                "a button affords activate"
            );
            assert!(
                node.action("focus").is_some(),
                "a focusable button affords focus"
            );
            assert!(matches!(
                node.action("activate").unwrap().kind,
                SemanticActionKind::Verb(Action::Activate)
            ));
        }

        // Case `plain_text_is_not_falsely_advertised_as_activatable`.
        {
            let label = crate::a11y::AccessNode::new(Role::Label);
            assert!(
                verbs_for(&label)
                    .iter()
                    .all(|action| action.name != "activate"),
                "the retained semantic tree must agree with AccessKit"
            );
        }

        // Case `declared_context_menu_is_projected_as_a_view_action`.
        {
            let row = crate::a11y::AccessNode::new(Role::ListItem)
                .with_state(AccessState::new().focusable(true).context_menu(true));
            let action = verbs_for(&row)
                .into_iter()
                .find(|action| action.name == "show-context-menu")
                .expect("declared context menus are discoverable");
            assert!(matches!(
                action.kind,
                SemanticActionKind::Verb(Action::ShowContextMenu)
            ));
        }

        // Case `effectful_controls_are_not_projected_without_a_domain_contract`.
        {
            let mut button = Button::new("go", "Go", None);
            let node_map = layout(&mut button);
            let mut tree = SemanticTree::new();
            tree.refresh(&button, &node_map, None, &[]);
            let resources = tree.action_resources();
            let go = resources.iter().find(|resource| resource.label == "Go");
            assert!(
                go.is_none_or(|resource| {
                    resource
                        .actions
                        .iter()
                        .all(|action| action.id != "activate")
                }),
                "generic activation must not bypass a domain action's permission and confirmation"
            );
        }

        // Case `standard_local_text_input_projects_its_framework_owned_set_value`.
        {
            let mut field = TextInput::new(Vec::new()).with_accessible_name("Destination");
            let node_map = layout(&mut field);
            let mut tree = SemanticTree::new();
            tree.refresh(&field, &node_map, None, &[]);
            let resources = tree.action_resources();
            let destination = resources
                .iter()
                .find(|resource| resource.label == "Destination");
            assert!(destination.is_some_and(|resource| {
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "set-value")
            }));
        }
    }

    #[test]
    fn refresh_deltas_track_focus_membership_and_nested_cells() {
        // Case `focus_change_updates_the_same_node`.
        {
            let mut col = Column::new()
                .push(Box::new(Button::new("a", "Alpha", None)))
                .push(Box::new(Button::new("b", "Beta", None)));
            let node_map = layout(&mut col);
            let first_button_taffy = node_map
                .iter()
                .find_map(|(t, &w)| {
                    let focusable =
                        unsafe { (*w).accessibility() }.is_some_and(|a| a.state.focusable);
                    focusable.then_some(*t)
                })
                .expect("a focusable button exists");

            let mut tree = SemanticTree::new();
            tree.refresh(&col, &node_map, None, &[]);
            let focused_id = tree
                .iter()
                .find(|node| node.widget_taffy_id == Some(u64::from(first_button_taffy)))
                .expect("button node present")
                .id
                .clone();
            let before = tree.node(&focused_id).expect("button node present").clone();
            assert!(!before.state.focused, "not focused initially");

            let delta = tree.refresh(&col, &node_map, Some(first_button_taffy), &[]);
            let after = tree.node(&focused_id).expect("button node still present");
            assert!(
                after.state.focused,
                "focus state reflected on the retained node"
            );
            assert_eq!(
                before.id, after.id,
                "identity preserved across the state change"
            );
            assert!(
                delta.updated.contains(&focused_id),
                "the focused node is in the delta, got {delta:?}"
            );
            assert!(
                delta.added.is_empty() && delta.removed.is_empty(),
                "only state changed"
            );
        }

        // Case `added_and_removed_nodes_surface_in_the_delta`.
        {
            let mut two = Column::new()
                .push(Box::new(Button::new("a", "Alpha", None)))
                .push(Box::new(Button::new("b", "Beta", None)));
            let map2 = layout(&mut two);
            let mut tree = SemanticTree::new();
            tree.refresh(&two, &map2, None, &[]);
            let baseline: std::collections::HashSet<SemanticId> =
                tree.iter().map(|n| n.id.clone()).collect();

            let mut one = Column::new().push(Box::new(Button::new("a", "Alpha", None)));
            let map1 = layout(&mut one);
            let delta = tree.refresh(&one, &map1, None, &[]);
            assert!(
                !delta.removed.is_empty(),
                "dropping a button reports a removal, got {delta:?}"
            );
            let now: std::collections::HashSet<SemanticId> =
                tree.iter().map(|n| n.id.clone()).collect();
            assert!(now.len() < baseline.len(), "the tree shrank");
        }

        // Case `retained_table_control_is_nested_under_its_synthetic_cell`.
        {
            struct RowData {
                id: u64,
            }
            let columns = vec![
                ColumnDef::new("actions", "Actions", ColumnWidth::Fixed(120.0)).with_widget(
                    |row: &RowData, _| {
                        let id = format!("edit-{}", row.id);
                        Box::new(Button::new(&id, "Edit", None))
                    },
                ),
            ];
            let mut table = DataTable::new(columns).with_row_key(|row: &RowData| row.id);
            table.set_rows(vec![RowData { id: 7 }]);
            let node_map = layout(&mut table);
            let mut tree = SemanticTree::new();
            tree.refresh(&table, &node_map, None, &[]);

            let table = tree
                .iter()
                .find(|node| node.role == Role::Table)
                .expect("table node");
            let row = tree
                .node(table.children.first().expect("table row id"))
                .expect("synthetic row");
            assert_eq!(row.role, Role::Row);
            let cell = tree
                .node(row.children.first().expect("row cell id"))
                .expect("synthetic cell");
            assert_eq!(cell.role, Role::Cell);
            let button = tree
                .node(cell.children.first().expect("cell control id"))
                .expect("retained button");
            assert_eq!(button.role, Role::Button);
            assert_eq!(button.name.as_deref(), Some("Edit"));
            assert_eq!(
                tree.iter().filter(|node| node.role == Role::Button).count(),
                1,
                "the retained control replaces, rather than duplicates, cell semantics"
            );
        }
    }
}
