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
//! the app's [`resources()`](crate::resources::resources) forest from
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
use tilo_sort::CompactSort as _;

use taffy::NodeId as TaffyNodeId;
use vello::kurbo::Rect;

use crate::a11y::{AccessNode, AccessState, Action, Role, SharedAccessNode};
use crate::widget::Widget;

#[cfg(test)]
#[path = "a11y_tree/identity_tests.rs"]
mod identity_tests;

#[cfg(test)]
#[path = "a11y_tree/continuation_tests.rs"]
mod continuation_tests;

#[cfg(test)]
#[path = "a11y_tree/retained_tests.rs"]
mod retained_tests;

#[path = "a11y_tree/continuation.rs"]
mod continuation;

#[path = "a11y_tree/node_identity.rs"]
mod node_identity;
mod reading_order;

#[path = "a11y_tree/component_nodes.rs"]
mod component_nodes;
use component_nodes::ComponentNodes;
pub(crate) use node_identity::AdmittedNodeNames;
use node_identity::WidgetNodeMap;
use reading_order::ReadingOrder;

type HostedWidget<'a> = (Vec<usize>, &'a dyn Widget);
type HostedByIndex<'a> = HashMap<usize, Vec<HostedWidget<'a>>>;
/// Ids are paths, as long as the tree is deep, and every node is hashed a few
/// times a refresh: a fast hash, not a DoS-resistant one, for keys no peer
/// chooses.
type NodeMap = HashMap<SemanticId, SemanticNode, rustc_hash::FxBuildHasher>;
type RetainedMap = HashMap<SemanticId, RetainedSubtree, rustc_hash::FxBuildHasher>;

/// The subtree a widget's synthetic child built at the last refresh: what it
/// was built from, and the ids of the nodes it built, in reading order.
///
/// Nothing else goes into those nodes. A child with the same id handed over
/// again, equal in every part, under the same owner and parent durability and
/// hosting no widget, builds exactly these nodes, so a refresh moves them over
/// from the last frame rather than building, diffing and dropping them: a key
/// typed into one paragraph rebuilds that paragraph's row, not every row of the
/// document. A child handed over as the very `Arc` it was retained from
/// ([`Widget::accessibility_shared`]) is that child unchanged without being
/// compared.
struct RetainedSubtree {
    access: Arc<AccessNode>,
    durable: bool,
    owner: Option<TaffyNodeId>,
    /// Shared with the [`ReadingOrder`] of every refresh that keeps it.
    ids: Arc<[SemanticId]>,
    /// Whether the refresh that holds it kept it rather than built it.
    kept: bool,
}
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionSpec, Collects, EncodedAction, OperationKind, Resource};

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
/// as long as the backing widget or resource recurs.
///
/// The string form encodes the node's origin so ids never collide between the
/// widget walk and the resource forest:
/// * `"root"` — the synthetic window root.
/// * `"w<taffy_id>"` — a laid-out widget, keyed by its `taffy::NodeId` (stable
///   between structural rebuilds, exactly the key the AccessKit bridge uses).
/// * `"<parent>~<index>"` — a synthetic a11y child (a table row/cell) with no
///   widget of its own, keyed by its position under its parent.
/// * `"r:<resource_id>"` — a resource from the app's action model, keyed by its
///   already-stable [`ResourceId`](tilo_actions::ResourceId).
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SemanticId(pub String);

impl std::borrow::Borrow<str> for SemanticId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl SemanticId {
    fn root() -> Self {
        SemanticId("root".into())
    }
    fn widget(taffy_id: TaffyNodeId) -> Self {
        SemanticId(format!("w{}", u64::from(taffy_id)))
    }
    fn synthetic(parent: &SemanticId, index: usize) -> Self {
        SemanticId(format!("{}~{}", parent.0, index))
    }
    fn keyed(parent: &SemanticId, key: impl std::fmt::Display) -> Self {
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
    /// frame. Layout ids and positional synthetic ids are deliberately not
    /// durable: exposing them to agents would let a later invocation target a
    /// different control after the UI is rebuilt.
    pub durable_identity: bool,
    pub role: Role,
    /// Already i18n-resolved.
    pub name: Option<String>,
    /// A field's contents, a slider's readout, etc.
    pub value: Option<String>,
    pub state: AccessState,
    /// `None` for the root and resource nodes. Virtual rows and cells publish
    /// their viewport geometry even though they have no retained widget.
    pub bounds: Option<Rect>,
    pub actions: Vec<SemanticAction>,
    /// In reading order.
    pub children: Vec<SemanticId>,
    /// So the host can route a [`Verb`](SemanticActionKind::Verb) back to
    /// the retained widget that owns this node.
    pub widget_taffy_id: Option<u64>,
    /// Opaque widget-local target for a synthetic child action.
    pub semantic_target: Option<String>,
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
            && self.state == other.state
            && self.bounds == other.bounds
            && self.actions == other.actions
            && self.children == other.children
            && self.semantic_target == other.semantic_target
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
    nodes: NodeMap,
    /// DFS reading order, so consumers can iterate the tree deterministically.
    order: ReadingOrder,
    root: SemanticId,
    /// Build closures for [`AppPrompted`](SemanticActionKind::AppPrompted)
    /// actions, keyed by `(node id, action name)`. Kept out of [`SemanticNode`]
    /// (a closure is neither `PartialEq` nor `Clone`-comparable) so nodes stay
    /// diffable; refreshed wholesale each frame.
    prompted: PromptedMap,
    bound_actions: HashMap<(SemanticId, String), crate::bound_dispatch::BoundActionTarget>,
    duplicate_ids: Vec<SemanticId>,
    /// Each bound component's resource nodes, in reading order after
    /// `order`. Kept apart so a refresh handed the same component snapshot
    /// reuses them rather than rebuilding and diffing a node per resource.
    components: Vec<ComponentNodes>,
    /// What each widget's synthetic children built at the last refresh, by
    /// the child's id: the next refresh moves over the nodes of those it is
    /// handed again unchanged.
    retained: RetainedMap,
    /// The nodes the last refresh built, rather than kept from the frame
    /// before.
    #[cfg(any(test, feature = "test-support"))]
    built: usize,
    /// The widgets' synthetic children the last refresh compared or built.
    #[cfg(any(test, feature = "test-support"))]
    compared: usize,
}

impl Default for SemanticTree {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticTree {
    pub fn new() -> Self {
        let root = SemanticId::root();
        let mut nodes = NodeMap::default();
        nodes.insert(
            root.clone(),
            SemanticNode {
                id: root.clone(),
                durable_identity: true,
                role: Role::Window,
                name: Some("Tilo application window".into()),
                value: None,
                state: AccessState::default(),
                bounds: None,
                actions: Vec::new(),
                children: Vec::new(),
                widget_taffy_id: None,
                semantic_target: None,
            },
        );
        Self {
            nodes,
            order: ReadingOrder::from_ids(vec![root.clone()]),
            root,
            prompted: HashMap::new(),
            bound_actions: HashMap::new(),
            duplicate_ids: Vec::new(),
            components: Vec::new(),
            retained: RetainedMap::default(),
            #[cfg(any(test, feature = "test-support"))]
            built: 0,
            #[cfg(any(test, feature = "test-support"))]
            compared: 0,
        }
    }

    /// The widget, declared-resource and bound-resource nodes the last
    /// refresh built, rather than kept from the frame before: what a refresh
    /// costs.
    #[cfg(any(test, feature = "test-support"))]
    pub fn nodes_built_for_test(&self) -> usize {
        self.built
    }

    /// The widgets' synthetic children the last refresh compared in full or
    /// built: every one a widget handed over other than as the very node the
    /// tree kept from it ([`Widget::accessibility_shared`]).
    #[cfg(any(test, feature = "test-support"))]
    pub fn subtrees_compared_for_test(&self) -> usize {
        self.compared
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
        self.refresh_impl::<false>(root, node_map, None, focused, resources, &[], false)
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
            false,
        )
    }
    /// Build only the durable semantic subtree consumed by action publication.
    /// Unkeyed ancestors make every descendant non-durable, so their entire
    /// branch is absent from `action_resources`, irrespective of layout/focus.
    /// The full accessibility refresh remains independent of this projection.
    pub(crate) fn refresh_action_projection(
        &mut self,
        root: &dyn Widget,
        node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
        admitted: &AdmittedNodeNames,
        focused: Option<TaffyNodeId>,
    ) -> TreeDelta {
        self.refresh_impl::<true>(root, node_map, Some(admitted), focused, &[], &[], true)
    }

    pub(crate) fn refresh_components(
        &mut self,
        root: &dyn Widget,
        node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
        admitted: &AdmittedNodeNames,
        focused: Option<TaffyNodeId>,
        resources: &[Resource],
        components: &[crate::bound_dispatch::BoundResourceSnapshot],
    ) -> TreeDelta {
        self.refresh_impl::<true>(
            root,
            node_map,
            Some(admitted),
            focused,
            resources,
            components,
            false,
        )
    }
    #[cfg(test)]
    pub(crate) fn bound_actions_for_test(
        &self,
    ) -> Vec<(SemanticId, crate::bound_dispatch::BoundActionTarget)> {
        self.bound_actions
            .iter()
            .chain(
                self.components
                    .iter()
                    .flat_map(|component| &component.bound_actions),
            )
            .map(|((id, _), target)| (id.clone(), target.clone()))
            .collect()
    }
    pub(crate) fn bound_target(
        &self,
        id: &SemanticId,
        action: &str,
    ) -> Option<crate::bound_dispatch::BoundActionTarget> {
        let key = (id.clone(), action.to_owned());
        self.bound_actions
            .get(&key)
            .or_else(|| {
                self.components
                    .iter()
                    .find_map(|component| component.bound_actions.get(&key))
            })
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
        actions_only: bool,
    ) -> TreeDelta {
        let ptr_to_taffy = WidgetNodeMap::new::<BOUND>(root, node_map, admitted);

        // The action projection keeps nothing between refreshes.
        let retained = if actions_only {
            RetainedMap::default()
        } else {
            std::mem::take(&mut self.retained)
        };
        let mut builder = Build::<BOUND>::new(&ptr_to_taffy, focused, actions_only, retained);

        let root_id = SemanticId::root();
        builder.order.push(root_id.clone());

        let mut root_children: Vec<SemanticId> = Vec::new();
        builder.visit_widget(root, &root_id, true, &mut root_children);

        // Resource affordances hang off the root as their own subtrees.
        for res in resources {
            let child = builder.visit_resource(res);
            root_children.push(child);
        }

        // Retained subtrees keep their nodes from the last frame where they
        // are, unless one of their ids was built afresh this frame. Only the
        // full walk reports such a repeat as it does, so it runs again, with
        // nothing retained to keep.
        if !self.carried_ids_hold(&builder) {
            return self.refresh_impl::<BOUND>(
                root,
                node_map,
                admitted,
                focused,
                resources,
                components,
                actions_only,
            );
        }

        // A component handing over the tree its nodes were built from keeps
        // them; one handing over a tree that shares all but some subtrees
        // with it updates the nodes of those; any other is built
        // afresh.
        let mut previous = std::mem::take(&mut self.components)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        let mut next_components = Vec::with_capacity(components.len());
        let mut reused = Vec::with_capacity(components.len());
        let mut updates = TreeDelta::default();
        #[cfg(any(test, feature = "test-support"))]
        let mut component_built = 0_usize;
        for component in components {
            let kept = previous
                .iter_mut()
                .find_map(|slot| slot.take_if(|nodes| nodes.built_from(component)));
            #[cfg(any(test, feature = "test-support"))]
            let kept_whole = kept.is_some();
            let kept = kept.or_else(|| {
                if actions_only {
                    return None;
                }
                let slot = previous.iter_mut().find(|slot| {
                    slot.as_ref()
                        .is_some_and(|nodes| nodes.same_owner(component))
                })?;
                let delta = slot.as_mut()?.update(component)?;
                updates.added.extend(delta.added);
                updates.removed.extend(delta.removed);
                updates.updated.extend(delta.updated);
                slot.take()
            });
            reused.push(kept.is_some());
            let nodes = kept.unwrap_or_else(|| ComponentNodes::build(component));
            #[cfg(any(test, feature = "test-support"))]
            if !kept_whole {
                component_built = component_built.saturating_add(nodes.built_for_test());
            }
            root_children.extend(nodes.roots().iter().cloned());
            next_components.push(nodes);
        }
        let previous = previous.into_iter().flatten().collect::<Vec<_>>();

        builder.nodes.insert(
            root_id.clone(),
            SemanticNode {
                id: root_id.clone(),
                durable_identity: true,
                role: Role::Window,
                name: Some("Tilo application window".into()),
                value: None,
                state: AccessState::default(),
                bounds: None,
                actions: Vec::new(),
                children: root_children,
                widget_taffy_id: None,
                semantic_target: None,
            },
        );

        // Keys are not escaped in semantic IDs. A key containing the path
        // separator can collide with a positional descendant in a pruned
        // branch; retain the full walk's duplicate rejection in that case.
        if actions_only && builder.ambiguous_projection {
            // The full walk below diffs against the last frame as it was.
            let mut restored = previous;
            restored.extend(
                next_components
                    .into_iter()
                    .zip(reused)
                    .filter_map(|(component, reused)| reused.then_some(component)),
            );
            self.components = restored;
            return self.refresh_impl::<BOUND>(
                root, node_map, admitted, focused, resources, components, false,
            );
        }
        let stale = self.stale_nodes(&builder);
        let mut delta = Sections {
            nodes: &self.nodes,
            components: &previous,
        }
        .diff(
            &Sections {
                nodes: &builder.nodes,
                components: &next_components,
            },
            &reused,
            &stale,
        );
        if !updates.added.is_empty() || !updates.removed.is_empty() || !updates.updated.is_empty() {
            delta.added.extend(updates.added);
            delta.removed.extend(updates.removed);
            delta.updated.extend(updates.updated);
            delta.added.compact_sort();
            delta.removed.compact_sort();
            delta.updated.compact_sort();
        }

        #[cfg(any(test, feature = "test-support"))]
        {
            self.built = builder.nodes.len().saturating_add(component_built);
            self.compared = builder.compared;
        }
        // The carried nodes are present and unchanged in both frames, in no
        // list of the delta. Where they are most of the tree (a keystroke),
        // they stay where they are and this frame's built nodes replace the
        // rest; where most was built again (a scroll), they move over.
        let carried = builder.order.kept_len();
        if builder.nodes.len() <= carried {
            for id in &stale {
                self.nodes.remove(id);
            }
            self.nodes.extend(std::mem::take(&mut builder.nodes));
        } else {
            let mut nodes = std::mem::take(&mut builder.nodes);
            for id in builder.order.kept() {
                if let Some((id, node)) = self.nodes.remove_entry(id) {
                    nodes.insert(id, node);
                }
            }
            self.nodes = nodes;
        }

        let mut duplicate_ids = builder.duplicate_ids;
        for component in &next_components {
            duplicate_ids.extend(component.duplicate_ids.iter().cloned());
        }
        self.retained = builder.retained;
        self.order = builder.order;
        self.root = root_id;
        self.prompted = builder.prompted;
        self.bound_actions = builder.bound_actions;
        self.duplicate_ids = duplicate_ids;
        self.components = next_components;
        delta
    }

    /// Whether the subtrees `builder` retained may keep their nodes: none of
    /// their ids was built afresh this frame, a repeated id the full walk has
    /// to report.
    ///
    /// Two retained subtrees never share an id: a subtree is retained only
    /// from a build that repeated none, and one that repeats a kept id fails
    /// here and is built again by the full walk. Each kept id is in the last
    /// frame's nodes, which keep every carried node in place.
    ///
    /// Asked of the built nodes, not the kept ones, so a keystroke asks it
    /// of the row it rebuilt: every id in a kept subtree is its root's id or
    /// extends it past a `~`, so only a built id one of whose `~`-prefixes
    /// is a kept root can be one of its ids.
    fn carried_ids_hold<const BOUND: bool>(&self, builder: &Build<'_, BOUND>) -> bool {
        #[cfg(debug_assertions)]
        for id in builder.order.kept() {
            debug_assert!(
                self.nodes.contains_key(id),
                "a retained subtree's node {id:?} is missing from the last frame"
            );
        }
        if builder.kept_overwritten {
            return false;
        }
        builder.nodes.keys().all(|id| {
            let text = id.as_str();
            text.match_indices('~')
                .filter_map(|(at, _)| text.get(..at))
                .chain(std::iter::once(text))
                .all(|prefix| {
                    builder
                        .retained
                        .get(prefix)
                        .filter(|subtree| subtree.kept)
                        .is_none_or(|subtree| !subtree.ids.contains(id))
                })
        })
    }

    /// The last frame's widget and declared-resource nodes this refresh
    /// neither kept nor built again. A frame that adds and removes no node
    /// has none, which counting tells without visiting the kept nodes: every
    /// last-frame node is kept, built again or stale.
    fn stale_nodes<const BOUND: bool>(&self, builder: &Build<'_, BOUND>) -> Vec<SemanticId> {
        let carried = builder.order.kept_len();
        let rebuilt = builder
            .nodes
            .keys()
            .filter(|id| self.nodes.contains_key(*id))
            .count();
        if self.nodes.len() == carried.saturating_add(rebuilt) {
            return Vec::new();
        }
        let carried: std::collections::HashSet<&SemanticId, rustc_hash::FxBuildHasher> =
            builder.order.kept().collect();
        self.nodes
            .keys()
            .filter(|id| !carried.contains(id) && !builder.nodes.contains_key(*id))
            .cloned()
            .collect()
    }

    /// What changed in the widget and declared-resource nodes if `next`
    /// replaced them.
    #[cfg(test)]
    fn diff(&self, next: &NodeMap) -> TreeDelta {
        Sections {
            nodes: &self.nodes,
            components: &[],
        }
        .diff(
            &Sections {
                nodes: next,
                components: &[],
            },
            &[],
            &self.nodes.keys().cloned().collect::<Vec<_>>(),
        )
    }

    /// Every node: the widget and declared-resource nodes, then each
    /// component's.
    fn all_nodes(&self) -> impl Iterator<Item = &SemanticNode> {
        self.nodes.values().chain(
            self.components
                .iter()
                .flat_map(|component| component.nodes.values()),
        )
    }

    pub fn root(&self) -> &SemanticId {
        &self.root
    }

    pub fn node(&self, id: &SemanticId) -> Option<&SemanticNode> {
        self.nodes.get(id).or_else(|| {
            self.components
                .iter()
                .find_map(|component| component.nodes.get(id))
        })
    }

    /// Includes the root.
    pub fn len(&self) -> usize {
        self.components
            .iter()
            .map(|component| component.nodes.len())
            .fold(self.nodes.len(), usize::saturating_add)
    }

    /// Whether the tree holds only the root.
    pub fn is_empty(&self) -> bool {
        self.len() <= 1
    }

    /// Stable DFS reading order.
    pub fn iter(&self) -> impl Iterator<Item = &SemanticNode> {
        self.order
            .iter()
            .filter_map(move |id| self.nodes.get(id))
            .chain(self.components.iter().flat_map(|component| {
                component
                    .order()
                    .iter()
                    .filter_map(move |id| component.nodes.get(id))
            }))
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
        let mut paths = self.control_paths_unranked(control_id);
        paths.compact_sort_by(|left, right| {
            left.len()
                .cmp(&right.len())
                .then_with(|| left.last().cmp(&right.last()))
        });
        paths
    }

    // Exact candidate enumeration shares the public path construction while
    // leaving shortest-path ranking to callers that need that policy.
    pub(crate) fn control_paths_unranked(&self, control_id: &str) -> Vec<Vec<SemanticId>> {
        if control_id.is_empty() {
            return Vec::new();
        }
        let mut parents = HashMap::<SemanticId, SemanticId>::new();
        for node in self.all_nodes() {
            for child in &node.children {
                parents.insert(child.clone(), node.id.clone());
            }
        }
        let marker = format!("~k:{control_id}");
        self.iter()
            .filter(|node| !node.id.0.starts_with("r:") && node.id.0.ends_with(&marker))
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
            .collect::<Vec<_>>()
    }

    pub fn action(&self, id: &SemanticId, name: &str) -> Option<&SemanticAction> {
        self.node(id).and_then(|n| n.action(name))
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
        let key = (id.clone(), name.to_string());
        self.prompted
            .get(&key)
            .or_else(|| {
                self.components
                    .iter()
                    .find_map(|component| component.prompted.get(&key))
            })
            .map(|build| build(arg))
    }
}

/// Mirrors the implicit/role actions
/// [`accesskit_bridge`](crate::accesskit_bridge) emits, so the retained
/// tree and the AT-SPI export agree on what a widget can do.
fn verbs_for(access: &AccessNode) -> Vec<SemanticAction> {
    verbs(access.role, &access.state)
}

/// [`verbs_for`] from the two parts of a node it reads.
fn verbs(role: Role, s: &AccessState) -> Vec<SemanticAction> {
    let mut out = Vec::new();
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
            role,
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
    match role {
        Role::Slider | Role::Separator | Role::SpinButton if !s.disabled => {
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
            if matches!(role, Role::Slider | Role::Separator) && !s.read_only {
                out.push(SemanticAction {
                    name: "set-value".into(),
                    label: "Set value".into(),
                    kind: SemanticActionKind::Verb(Action::SetValue(String::new())),
                });
            }
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
            if matches!(role, Role::ComboBox) && !s.read_only && !s.disabled {
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

/// Separate from [`SemanticTree`] so the retained frame is only replaced
/// once the new one is fully built (and diffed against).
struct Build<'a, const BOUND: bool> {
    actions_only: bool,
    ambiguous_projection: bool,
    nodes: NodeMap,
    order: ReadingOrder,
    prompted: PromptedMap,
    bound_actions: HashMap<(SemanticId, String), crate::bound_dispatch::BoundActionTarget>,
    ptr_to_taffy: &'a WidgetNodeMap<'a>,
    focused: Option<TaffyNodeId>,
    duplicate_ids: Vec<SemanticId>,
    /// The last refresh's retained subtrees, taken from as they recur.
    retained_before: RetainedMap,
    /// This refresh's, for the next.
    retained: RetainedMap,
    /// A subtree built this refresh took the place of one it kept under the
    /// same id: a repeated id the full walk has to report.
    kept_overwritten: bool,
    /// The synthetic children compared in full or built.
    #[cfg(any(test, feature = "test-support"))]
    compared: usize,
}

/// A widget's synthetic child as the widget handed it over: built this
/// frame, or shared with the frame that built it.
enum Synthetic {
    Owned(AccessNode),
    Shared(Arc<AccessNode>),
}

impl Synthetic {
    fn node(&self) -> &AccessNode {
        match self {
            Self::Owned(node) => node,
            Self::Shared(node) => node,
        }
    }

    /// Whether this child is the very node `retained` was retained from.
    #[cfg(any(test, feature = "test-support"))]
    fn is_shared(&self, retained: &Arc<AccessNode>) -> bool {
        matches!(self, Self::Shared(node) if Arc::ptr_eq(node, retained))
    }

    /// Whether this child is `retained`'s: the very node it was retained
    /// from, or one equal to it in every part.
    fn is(&self, retained: &Arc<AccessNode>) -> bool {
        match self {
            Self::Owned(node) => **retained == *node,
            Self::Shared(node) => Arc::ptr_eq(node, retained) || **node == **retained,
        }
    }

    fn into_shared(self) -> Arc<AccessNode> {
        match self {
            Self::Owned(node) => Arc::new(node),
            Self::Shared(node) => node,
        }
    }
}

/// The node a synthetic child hangs off: its id, and whether that id is
/// durable (a keyed id is durable only under a durable parent).
#[derive(Clone, Copy)]
struct SyntheticParent<'a> {
    id: &'a SemanticId,
    durable: bool,
}

/// One frame's nodes as the tree keeps them: the widget and declared-resource
/// nodes, and each bound component's.
struct Sections<'a> {
    nodes: &'a NodeMap,
    components: &'a [ComponentNodes],
}

impl Sections<'_> {
    fn get(&self, id: &SemanticId) -> Option<&SemanticNode> {
        self.nodes.get(id).or_else(|| {
            self.components
                .iter()
                .find_map(|component| component.nodes.get(id))
        })
    }

    /// What changed from these (the last frame's) to `next`. `reused[i]`
    /// marks `next.components[i]` as the very nodes of the last frame, moved
    /// over unchanged, so they are neither compared nor listed; the last
    /// frame's components here are only the ones that were not. Of the last
    /// frame's widget and declared-resource nodes, only the `stale` ones (in
    /// neither `next.nodes` nor the retained subtrees) may be gone.
    fn diff(&self, next: &Sections<'_>, reused: &[bool], stale: &[SemanticId]) -> TreeDelta {
        let mut delta = TreeDelta::default();
        let rebuilt = next
            .components
            .iter()
            .zip(reused)
            .filter(|(_, reused)| !**reused)
            .flat_map(|(component, _)| &component.nodes);
        for (id, node) in next.nodes.iter().chain(rebuilt) {
            match self.get(id) {
                None => delta.added.push(id.clone()),
                Some(prev) if !prev.same_exported_content(node) => delta.updated.push(id.clone()),
                Some(_) => {}
            }
        }
        let gone = self
            .components
            .iter()
            .flat_map(|component| component.nodes.keys());
        for id in stale.iter().chain(gone) {
            if next.get(id).is_none() {
                delta.removed.push(id.clone());
            }
        }
        delta.added.compact_sort();
        delta.removed.compact_sort();
        delta.updated.compact_sort();
        delta
    }
}

impl<'a, const BOUND: bool> Build<'a, BOUND> {
    fn new(
        ptr_to_taffy: &'a WidgetNodeMap<'a>,
        focused: Option<TaffyNodeId>,
        actions_only: bool,
        retained_before: RetainedMap,
    ) -> Self {
        Self {
            // As many as last time, most of them the same ones: room for
            // them up front rather than rehashing as they recur.
            retained: RetainedMap::with_capacity_and_hasher(
                retained_before.len(),
                rustc_hash::FxBuildHasher,
            ),
            retained_before,
            kept_overwritten: false,
            #[cfg(any(test, feature = "test-support"))]
            compared: 0,
            nodes: NodeMap::default(),
            order: ReadingOrder::default(),
            prompted: HashMap::new(),
            bound_actions: HashMap::new(),
            duplicate_ids: Vec::new(),
            actions_only,
            ambiguous_projection: false,
            ptr_to_taffy,
            focused,
        }
    }

    fn insert_node(&mut self, node: SemanticNode) {
        let id = node.id.clone();
        if self.nodes.insert(id.clone(), node).is_some() {
            self.duplicate_ids.push(id);
        }
    }
    /// A presentational widget (`accessibility() == None`) is collapsed
    /// out and its children reparented — matching the AccessKit bridge.
    ///
    /// A keyed id embeds its parent's, so it is durable only under a
    /// durable parent: a key under a layout id moves when the UI rebuilds.
    fn visit_widget(
        &mut self,
        w: &dyn Widget,
        semantic_parent: &SemanticId,
        parent_durable: bool,
        parent_children: &mut Vec<SemanticId>,
    ) {
        let Ok(_entry) = crate::bound_dispatch::enter_traversal::<BOUND>(w) else {
            return;
        };
        let taffy_id = self.ptr_to_taffy.get(w);
        let (access, shared) = match w.accessibility_shared() {
            Some(SharedAccessNode { node, children }) => (Some(node), children),
            None => (w.accessibility(), Vec::new()),
        };
        match access {
            Some(mut access) => {
                let id = if let Some(key) = &access.semantic_key {
                    SemanticId::keyed(semantic_parent, key)
                } else if let Some(t) = taffy_id {
                    SemanticId::widget(t)
                } else {
                    // Rare (never laid out); key under the parent so it
                    // still gets a stable id.
                    let root = SemanticId::root();
                    let parent = parent_children.last().unwrap_or(&root);
                    SemanticId::synthetic(parent, parent_children.len())
                };
                let durable_identity = access.semantic_key.is_some() && parent_durable;
                self.ambiguous_projection |= durable_identity
                    && access
                        .semantic_key
                        .as_ref()
                        .is_some_and(|key| key.contains('~'));
                if self.actions_only && !durable_identity {
                    return;
                }

                let mut children: Vec<SemanticId> = Vec::new();
                let mut hosted = HostedByIndex::new();
                for (child_index, child) in w.children().into_iter().enumerate() {
                    if w.accessibility_represents_child(child_index) {
                        continue;
                    }
                    if let Some(mut path) = w.accessibility_child_path(child_index)
                        && !path.is_empty()
                    {
                        let parent_index = path.remove(0);
                        hosted.entry(parent_index).or_default().push((path, child));
                    } else {
                        self.visit_widget(child, &id, durable_identity, &mut children);
                    }
                }
                let synthetic = std::mem::take(&mut access.children)
                    .into_iter()
                    .map(Synthetic::Owned)
                    .chain(shared.into_iter().map(Synthetic::Shared));
                for (i, synth) in synthetic.enumerate() {
                    let child = self.visit_synthetic_retained(
                        SyntheticParent {
                            id: &id,
                            durable: durable_identity,
                        },
                        i,
                        synth,
                        taffy_id,
                        hosted.remove(&i).unwrap_or_default(),
                    );
                    children.push(child);
                }

                let mut state = access.state;
                if let (Some(t), Some(f)) = (taffy_id, self.focused)
                    && t == f
                {
                    state.focused = true;
                }

                // Disabled/read-only gate the verbs but focus does not, so
                // compute against the focus-updated state.
                let actions = verbs(access.role, &state);

                let node = SemanticNode {
                    id: id.clone(),
                    durable_identity,
                    role: access.role,
                    name: access.name.clone(),
                    value: if access.state.protected {
                        None
                    } else {
                        access.value.clone()
                    },
                    state,
                    bounds: Some(w.get_rect()),
                    actions,
                    children,
                    widget_taffy_id: taffy_id.map(u64::from),
                    semantic_target: access.action_target.clone(),
                };
                self.order.push(id.clone());
                self.insert_node(node);
                parent_children.push(id);
            }
            None => {
                for child in w.children() {
                    self.visit_widget(child, semantic_parent, parent_durable, parent_children);
                }
            }
        }
    }

    /// A widget's synthetic child: its nodes moved over from the last frame
    /// when it built them from the same parts ([`RetainedSubtree`]), else
    /// [`visit_synthetic`](Self::visit_synthetic) builds them and they are
    /// retained for the next.
    fn visit_synthetic_retained(
        &mut self,
        parent: SyntheticParent<'_>,
        index: usize,
        access: Synthetic,
        owner: Option<TaffyNodeId>,
        hosted: Vec<HostedWidget<'_>>,
    ) -> SemanticId {
        // A widget hosted in the child is walked afresh, and the projection
        // for actions keeps nothing.
        if !hosted.is_empty() || self.actions_only {
            #[cfg(any(test, feature = "test-support"))]
            {
                self.compared = self.compared.saturating_add(1);
            }
            return self.visit_synthetic(parent, index, access.node(), owner, hosted);
        }
        let id = access.node().key.map_or_else(
            || SemanticId::synthetic(parent.id, index),
            |key| SemanticId::keyed(parent.id, key),
        );
        if let Some(retained) = self.retained_before.remove(&id)
            && retained.durable == parent.durable
            && retained.owner == owner
            && access.is(&retained.access)
        {
            #[cfg(any(test, feature = "test-support"))]
            if !access.is_shared(&retained.access) {
                self.compared = self.compared.saturating_add(1);
            }
            self.order.keep(&retained.ids);
            self.retained.insert(
                id.clone(),
                RetainedSubtree {
                    kept: true,
                    ..retained
                },
            );
            return id;
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            self.compared = self.compared.saturating_add(1);
        }
        let start = self.order.mark();
        let duplicates = self.duplicate_ids.len();
        let built = self.visit_synthetic(parent, index, access.node(), owner, hosted);
        // A subtree that repeated an id is left to the full walk.
        if built == id && self.duplicate_ids.len() == duplicates {
            let ids = self.order.since(start).into();
            let replaced = self.retained.insert(
                id,
                RetainedSubtree {
                    access: access.into_shared(),
                    durable: parent.durable,
                    owner,
                    ids,
                    kept: false,
                },
            );
            self.kept_overwritten |= replaced.is_some_and(|subtree| subtree.kept);
        }
        built
    }

    /// A virtual row/cell — has no backing widget, but may carry geometry.
    /// The parent's durability is passed down rather than read from
    /// `self.nodes`: a parent is stored only after its children are visited.
    fn visit_synthetic(
        &mut self,
        SyntheticParent {
            id: parent,
            durable: parent_durable,
        }: SyntheticParent<'_>,
        index: usize,
        access: &AccessNode,
        owner: Option<TaffyNodeId>,
        hosted: Vec<HostedWidget<'_>>,
    ) -> SemanticId {
        let id = access.key.map_or_else(
            || SemanticId::synthetic(parent, index),
            |key| SemanticId::keyed(parent, key),
        );
        let durable_identity = access.key.is_some() && parent_durable;
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
                    SyntheticParent {
                        id: &id,
                        durable: durable_identity,
                    },
                    i,
                    child,
                    owner,
                    nested.remove(&i).unwrap_or_default(),
                );
                children.push(c);
            }
        } else {
            for child in direct {
                self.visit_widget(child, &id, durable_identity, &mut children);
            }
        }
        let node = SemanticNode {
            id: id.clone(),
            durable_identity,
            role: access.role,
            name: access.name.clone(),
            value: if access.state.protected {
                None
            } else {
                access.value.clone()
            },
            state: access.state,
            bounds: access.bounds.map(crate::a11y::AccessBounds::to_rect),
            actions: if semantic_target.is_some() {
                verbs_for(access)
            } else {
                Vec::new()
            },
            children,
            widget_taffy_id: semantic_target.and(owner).map(u64::from),
            semantic_target: semantic_target.map(str::to_owned),
        };
        self.order.push(id.clone());
        self.insert_node(node);
        id
    }

    /// A declared resource's node and its children's, beneath the root.
    fn visit_resource(&mut self, res: &Resource) -> SemanticId {
        let id = SemanticId::resource(res.id.as_str());
        let actions = component_nodes::resource_actions(&id, res, None, &mut self.prompted, None);
        let mut children: Vec<SemanticId> = Vec::new();
        for child in &res.children {
            children.push(self.visit_resource(child));
        }
        let node =
            component_nodes::resource_semantic_node(id.clone(), res, actions, children, true);
        self.order.push(id.clone());
        self.insert_node(node);
        id
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

    include!("a11y_tree/action_projection_tests.rs");

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
    fn sidebar_range_controls_publish_existing_set_value_alongside_steps() {
        use crate::widget::{Slider, SplitHandle, SplitOrientation, SplitRatio};
        let slider = Slider::new("volume", "volume").with_accessible_name("Volume");
        let separator = SplitHandle::new(SplitOrientation::Horizontal, SplitRatio::default());
        let controls = [
            slider.accessibility().unwrap(),
            SplitHandle::accessibility(&separator),
        ];
        for mut access in controls {
            let actions = verbs_for(&access);
            for verb in ["increment", "decrement", "set-value"] {
                assert!(actions.iter().any(|action| action.name == verb));
            }
            access.state.read_only = true;
            assert!(
                verbs_for(&access)
                    .iter()
                    .all(|action| action.name != "set-value")
            );
            access.state.disabled = true;
            assert!(verbs_for(&access).is_empty());
        }
        let mut slider = slider;
        let map = layout(&mut slider);
        let mut tree = SemanticTree::new();
        tree.refresh(&slider, &map, None, &[]);
        let resources = tree.action_resources();
        let volume = resources
            .iter()
            .find(|resource| resource.label == "Volume")
            .unwrap();
        assert!(volume.actions.iter().any(|action| action.id == "set-value"));
        assert!(
            tilo_actions::validate_surface_contracts(
                &resources,
                &tilo_actions::ActionRegistry::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn semantic_ids_are_durable_and_exported_only_when_stable() {
        // Case `identity_is_stable_across_frames`.
        {
            let mut col = Column::new()
                .push(Box::new(Button::new("a", "Alpha")))
                .push(Box::new(Button::new("b", "Beta")));
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
                ids_first.iter().any(|id| id.as_str().contains("~k:a"))
                    && ids_first.iter().any(|id| id.as_str().contains("~k:b")),
                "buttons use their durable widget keys, got {ids_first:?}"
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
                id,
                durable_identity: true,
                role: Role::Button,
                name: Some("Save".into()),
                value: None,
                state: AccessState::default(),
                bounds: Some(Rect::new(0.0, 0.0, 10.0, 10.0)),
                actions: Vec::new(),
                children,
                widget_taffy_id: None,
                semantic_target: None,
            };
            tree.nodes
                .insert(group.clone(), node(group.clone(), vec![deep.clone()]));
            tree.nodes
                .insert(deep.clone(), node(deep.clone(), Vec::new()));
            tree.nodes
                .insert(shallow.clone(), node(shallow.clone(), Vec::new()));
            tree.nodes.get_mut(&tree.root).unwrap().children = vec![group.clone(), shallow.clone()];
            tree.order = ReadingOrder::from_ids(vec![
                tree.root.clone(),
                group,
                deep.clone(),
                shallow.clone(),
            ]);

            let routes = tree.control_paths("save");
            assert_eq!(routes.len(), 2);
            assert_eq!(routes[0], vec![tree.root.clone(), shallow]);
            assert_eq!(routes[1].last(), Some(&deep));
        }

        // Case `durable_widget_keys_survive_layout_rebuild_and_sibling_reorder`.
        {
            let mut first = Column::new()
                .push(Box::new(Button::new("alpha", "Alpha")))
                .push(Box::new(Button::new("beta", "Beta")));
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
            assert!(alpha.as_str().contains("~k:alpha"));

            let mut rebuilt = Column::new()
                .push(Box::new(Button::new("unrelated", "Unrelated")))
                .push(Box::new(Button::new("beta", "Beta")))
                .push(Box::new(Button::new("alpha", "Alpha")));
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
            let mut button = Button::new("settings.section:network\u{1f}open", "Network");
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
            let mut button = Button::new("go", "Go");
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

        // Case `controls_in_a_scroll_region_are_published_only_under_a_key`.
        {
            let published = |scroll: crate::widget::Scrollable| {
                let mut scroll = scroll.with_child(Box::new(Button::new("go", "Go")));
                let node_map = layout(&mut scroll);
                let mut tree = SemanticTree::new();
                tree.refresh(&scroll, &node_map, None, &[]);
                tree.action_resources()
                    .iter()
                    .map(|resource| resource.id.as_str().to_owned())
                    .collect::<Vec<_>>()
            };
            assert!(
                published(crate::widget::Scrollable::new_vertical()).is_empty(),
                "an unkeyed scroll group follows its layout node"
            );
            let keyed =
                published(crate::widget::Scrollable::new_vertical().with_semantic_key("pane"));
            assert!(
                keyed
                    .iter()
                    .any(|id| id.contains("pane") && id.contains("go")),
                "a keyed scroll group keeps its controls durable, got {keyed:?}"
            );
        }
    }

    #[test]
    fn unranked_control_paths_preserve_public_shortest_path_order() {
        let mut root = Column::new()
            .push(Box::new(
                crate::widget::Scrollable::new_vertical()
                    .with_semantic_key("advanced")
                    .with_child(Box::new(Button::new("save", "Deep save"))),
            ))
            .push(Box::new(Button::new("save", "Shallow save")));
        let nodes = layout(&mut root);
        let mut tree = SemanticTree::new();
        tree.refresh(&root, &nodes, None, &[]);
        let unranked = tree.control_paths_unranked("save");
        assert_eq!(unranked.len(), 2);
        let deep = tree.find_by_name("Deep save").unwrap().id.clone();
        let shallow = tree.find_by_name("Shallow save").unwrap().id.clone();
        assert_eq!(unranked[0].last(), Some(&deep));
        assert_eq!(unranked[1].last(), Some(&shallow));
        let ranked = tree.control_paths("save");
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0], vec![tree.root.clone(), shallow]);
        assert_eq!(ranked[1].len(), 3);
        assert_eq!(ranked[1].last(), Some(&deep));
        assert!(unranked.contains(&ranked[0]));
        assert!(unranked.contains(&ranked[1]));
        assert!(tree.control_paths_unranked("missing").is_empty());
        assert!(tree.control_paths("missing").is_empty());
        assert!(tree.control_paths_unranked("").is_empty());
        assert!(tree.control_paths("").is_empty());
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
            let mut btn = Button::new("go", "Go");
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
            let mut button = Button::new("go", "Go");
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
                .push(Box::new(Button::new("a", "Alpha")))
                .push(Box::new(Button::new("b", "Beta")));
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
                .push(Box::new(Button::new("a", "Alpha")))
                .push(Box::new(Button::new("b", "Beta")));
            let map2 = layout(&mut two);
            let mut tree = SemanticTree::new();
            tree.refresh(&two, &map2, None, &[]);
            let baseline: std::collections::HashSet<SemanticId> =
                tree.iter().map(|n| n.id.clone()).collect();

            let mut one = Column::new().push(Box::new(Button::new("a", "Alpha")));
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
                        Box::new(Button::new(&id, "Edit"))
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
