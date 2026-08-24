//! The **retained accessibility tree** — a stable, frame-to-frame semantic model
//! of the running UI.
//!
//! ## Why retained
//!
//! The toolkit is immediate-mode: every frame `draw` rebuilds the scene from
//! scratch, and [`accesskit_bridge::build_tree_update`](crate::accesskit_bridge::build_tree_update)
//! likewise re-derives a *fresh* [`accesskit::TreeUpdate`] each call. That is
//! correct for pushing a whole tree at AccessKit (its platform adapters suppress
//! unchanged nodes), but it gives no **stable identity** and no **diff**: nothing
//! survives between frames, so a consumer that wants to say "node 7's *state*
//! changed" or "these two nodes appeared" has to compare two full trees itself.
//!
//! A [`SemanticTree`] is the retained counterpart. It holds the previous frame's
//! nodes keyed by a **stable [`SemanticId`]** and, on each
//! [`refresh`](SemanticTree::refresh), diffs the freshly-walked UI against them,
//! returning a [`TreeDelta`] (added / removed / updated) while preserving the
//! identity of every node that recurs. Unchanged nodes keep their exact identity
//! and content; only what actually moved shows up in the delta. This is the
//! foundation an AT-SPI incremental push, a screen reader's change narration, or
//! an AI-agent / CLI introspection layer builds on.
//!
//! ## Two sources, one tree
//!
//! Each refresh assembles the tree from the two models the toolkit already
//! maintains, so a node carries both *what it is* and *what you can do to it*:
//!
//! 1. **Widget structure** — the [`Widget::accessibility`](crate::widget::Widget::accessibility)
//!    walk (roles, names, values, [`AccessState`], bounds), collapsing
//!    presentational widgets exactly like the AccessKit bridge. Each such node
//!    also carries the **generic verbs** its state affords (focus, activate,
//!    increment/decrement, expand/collapse, set-value) as
//!    [`SemanticActionKind::Verb`] affordances routed through
//!    [`Widget::perform_action`](crate::widget::Widget::perform_action).
//!
//! 2. **Resource affordances** — the app's own
//!    [`resources()`](crate::widget::Widget::resources) forest from the qOS
//!    [action model](qos_actions). Every [`ActionSpec`](qos_actions::ActionSpec) an
//!    open resource exports becomes a [`SemanticActionKind::App`] affordance
//!    carrying the **exact [`EncodedAction`](qos_actions::EncodedAction)** the
//!    on-screen control and the Ctrl+K palette dispatch. Invoking it by name
//!    therefore drives the *same* replayable action a real click does — which is
//!    what lets an assistive client (or an agent) not just *read* the tree but
//!    *operate* it through the one audited dispatch path.
//!
//! The tree itself imports no AccessKit and no D-Bus — it is pure data plus a
//! diff, unit-testable without a bus or a window.

use std::collections::HashMap;
use std::sync::Arc;

use taffy::NodeId as TaffyNodeId;
use vello::kurbo::Rect;

use crate::a11y::{AccessNode, AccessState, Action, Role};
use crate::widget::Widget;
use qos_actions::{ActionInput, ActionSpec, Collects, EncodedAction, Resource};

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
///   already-stable [`ResourceId`](qos_actions::ResourceId).
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SemanticId(pub String);

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
    fn resource(id: &str) -> Self {
        SemanticId(format!("r:{id}"))
    }

    /// The raw string key.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a [`SemanticAction`] is carried out when invoked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticActionKind {
    /// A generic accessibility verb ([`Action`]) — focus, activate,
    /// increment/decrement, expand/collapse, set-value. Routed through
    /// [`Widget::perform_action`](crate::widget::Widget::perform_action) with the
    /// `App`'s generic focus/click fallback, exactly like an AccessKit
    /// `ActionRequest`.
    Verb(Action),
    /// An app action from the qOS [action model](qos_actions), ready to fire.
    /// `encoded` is the byte-identical [`EncodedAction`] the on-screen affordance
    /// and the Ctrl+K palette dispatch — invoking it replays the *same* action.
    App(EncodedAction),
    /// An app action that first needs one free-text argument (a rename, a "new
    /// page named…"). The argument is supplied to
    /// [`SemanticTree::build_prompted`] to produce the concrete [`EncodedAction`].
    AppPrompted {
        /// The prompt shown when collecting the argument.
        prompt: String,
        /// Placeholder text for the argument field.
        placeholder: String,
        /// What that argument names — words somebody says, or the address of
        /// content that already exists. Carried rather than dropped because
        /// this tree's whole audience is callers with no screen, and that is
        /// the distinction [`Collects`] exists to make for them: offered a
        /// text field for a [`Collects::Content`] argument, a client can only
        /// type something that was never going to resolve.
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
    /// Stable identity across frames.
    pub id: SemanticId,
    /// Semantic role.
    pub role: Role,
    /// Accessible name (already i18n-resolved).
    pub name: Option<String>,
    /// Current value as text (a field's contents, a slider's readout).
    pub value: Option<String>,
    /// Live boolean/tri-state flags.
    pub state: AccessState,
    /// Logical bounds, when the node is a laid-out widget. `None` for the root,
    /// synthetic a11y children, and resource nodes (none of which are laid out).
    pub bounds: Option<Rect>,
    /// The affordances this node exposes.
    pub actions: Vec<SemanticAction>,
    /// Child node ids, in reading order.
    pub children: Vec<SemanticId>,
    /// The backing widget's `taffy::NodeId`, when this node is a widget — so the
    /// host can route a [`Verb`](SemanticActionKind::Verb) back to it. `None` for
    /// root / synthetic / resource nodes.
    pub widget_taffy_id: Option<u64>,
}

impl SemanticNode {
    /// Find one of this node's [`actions`](Self::actions) by its stable name.
    pub fn action(&self, name: &str) -> Option<&SemanticAction> {
        self.actions.iter().find(|a| a.name == name)
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
    /// Whether nothing changed at all (a stable frame).
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.updated.is_empty()
    }
}

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
    prompted: HashMap<(SemanticId, String), Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>>,
}

impl Default for SemanticTree {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticTree {
    /// A fresh, empty tree (just the synthetic root, with no children yet).
    pub fn new() -> Self {
        let root = SemanticId::root();
        let mut nodes = HashMap::new();
        nodes.insert(
            root.clone(),
            SemanticNode {
                id: root.clone(),
                role: Role::Window,
                name: Some("qOS application window".into()),
                value: None,
                state: AccessState::default(),
                bounds: None,
                actions: Vec::new(),
                children: Vec::new(),
                widget_taffy_id: None,
            },
        );
        Self {
            nodes,
            order: vec![root.clone()],
            root,
            prompted: HashMap::new(),
        }
    }

    /// Rebuild the tree from the live UI and diff it against the retained frame.
    ///
    /// * `root` — the App's root widget.
    /// * `node_map` — the App's `taffy::NodeId -> *mut dyn Widget` map, used
    ///   *read-only* (never dereferenced) to assign each widget its stable id,
    ///   exactly as [`accesskit_bridge`](crate::accesskit_bridge) does.
    /// * `focused` — the focused widget's `taffy::NodeId`, folded into each
    ///   node's [`AccessState::focused`].
    /// * `resources` — the focused app's open-resource forest
    ///   ([`Widget::resources`](crate::widget::Widget::resources)); each exported
    ///   [`ActionSpec`](qos_actions::ActionSpec) becomes an invokable affordance.
    ///
    /// Returns the [`TreeDelta`]. Node identity is preserved for every id that
    /// recurs; the internal store is replaced but the *identities* are stable.
    pub fn refresh(
        &mut self,
        root: &dyn Widget,
        node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
        focused: Option<TaffyNodeId>,
        resources: &[Resource],
    ) -> TreeDelta {
        // Reverse map widget *data* pointer -> taffy id. We key on the thin data
        // address (`*const ()`), not the fat `*const dyn Widget`, so the lookup is
        // immune to the vtable-pointer differences that can arise between a
        // widget's `self`-coercion (stored in `node_map` during `build_layout`)
        // and the `&dyn Widget` its parent hands back from `children()`. The data
        // address of a live object is identical across every coercion of it.
        let mut ptr_to_taffy: HashMap<*const (), TaffyNodeId> = HashMap::new();
        for (&taffy_id, &w_ptr) in node_map {
            ptr_to_taffy.insert(w_ptr as *const (), taffy_id);
        }

        let mut builder = Build {
            nodes: HashMap::new(),
            order: Vec::new(),
            prompted: HashMap::new(),
            ptr_to_taffy: &ptr_to_taffy,
            focused,
        };

        let root_id = SemanticId::root();
        builder.order.push(root_id.clone());

        // 1. Widget structure hangs off the root.
        let mut root_children: Vec<SemanticId> = Vec::new();
        builder.visit_widget(root, &mut root_children);

        // 2. Resource affordances hang off the root as their own subtrees.
        for res in resources {
            let child = builder.visit_resource(res);
            root_children.push(child);
        }

        builder.nodes.insert(
            root_id.clone(),
            SemanticNode {
                id: root_id.clone(),
                role: Role::Window,
                name: Some("qOS application window".into()),
                value: None,
                state: AccessState::default(),
                bounds: None,
                actions: Vec::new(),
                children: root_children,
                widget_taffy_id: None,
            },
        );

        // Diff the freshly-built frame against the retained one.
        let delta = self.diff(&builder.nodes);

        self.nodes = builder.nodes;
        self.order = builder.order;
        self.root = root_id;
        self.prompted = builder.prompted;
        delta
    }

    fn diff(&self, next: &HashMap<SemanticId, SemanticNode>) -> TreeDelta {
        let mut delta = TreeDelta::default();
        for (id, node) in next {
            match self.nodes.get(id) {
                None => delta.added.push(id.clone()),
                Some(prev) if prev != node => delta.updated.push(id.clone()),
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

    /// The root node's id.
    pub fn root(&self) -> &SemanticId {
        &self.root
    }

    /// Look up a node by id.
    pub fn node(&self, id: &SemanticId) -> Option<&SemanticNode> {
        self.nodes.get(id)
    }

    /// Number of nodes (including the root).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree holds only the root.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// Every node in stable DFS reading order.
    pub fn iter(&self) -> impl Iterator<Item = &SemanticNode> {
        self.order.iter().filter_map(move |id| self.nodes.get(id))
    }

    /// The first node whose accessible [`name`](SemanticNode::name) equals
    /// `name`, in reading order — the ergonomic handle for a CLI / agent that
    /// addresses elements by label ("focus the node called 'Volume'").
    pub fn find_by_name(&self, name: &str) -> Option<&SemanticNode> {
        self.iter().find(|n| n.name.as_deref() == Some(name))
    }

    /// Look up a node's action by the node id and the action's stable name.
    pub fn action(&self, id: &SemanticId, name: &str) -> Option<&SemanticAction> {
        self.nodes.get(id).and_then(|n| n.action(name))
    }

    /// Build the concrete [`EncodedAction`] for an
    /// [`AppPrompted`](SemanticActionKind::AppPrompted) action from its
    /// collected `arg`. Returns `None` if `(id, name)` is not a prompted action.
    pub fn build_prompted(&self, id: &SemanticId, name: &str, arg: &str) -> Option<EncodedAction> {
        self.prompted
            .get(&(id.clone(), name.to_string()))
            .map(|build| build(arg))
    }
}

/// The generic accessibility verbs an [`AccessNode`]'s role + state afford —
/// mirroring the implicit/role actions [`accesskit_bridge`](crate::accesskit_bridge)
/// emits, so the retained tree and the AT-SPI export agree on what a widget can do.
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
    if !s.disabled {
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

/// Per-refresh accumulator for the DFS build. Separate from [`SemanticTree`] so
/// the retained frame is only replaced once the new one is fully built (and
/// diffed against).
struct Build<'a> {
    nodes: HashMap<SemanticId, SemanticNode>,
    order: Vec<SemanticId>,
    prompted: HashMap<(SemanticId, String), Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>>,
    ptr_to_taffy: &'a HashMap<*const (), TaffyNodeId>,
    focused: Option<TaffyNodeId>,
}

impl Build<'_> {
    /// Visit a widget. If it exposes semantics, emit a node and append its id to
    /// `parent_children`; if presentational (`accessibility() == None`), collapse
    /// it out and reparent its children — matching the AccessKit bridge.
    fn visit_widget(&mut self, w: &dyn Widget, parent_children: &mut Vec<SemanticId>) {
        let taffy_id = self.ptr_to_taffy.get(&(w as *const dyn Widget as *const ())).copied();
        match w.accessibility() {
            Some(access) => {
                let id = match taffy_id {
                    Some(t) => SemanticId::widget(t),
                    // A semantic widget that was never laid out is rare; key it
                    // under the parent so it still gets a stable id.
                    None => SemanticId::synthetic(
                        parent_children.last().unwrap_or(&SemanticId::root()),
                        parent_children.len(),
                    ),
                };

                let mut children: Vec<SemanticId> = Vec::new();
                for child in w.children() {
                    self.visit_widget(child, &mut children);
                }
                for (i, synth) in access.children.iter().enumerate() {
                    let child = self.visit_synthetic(&id, i, synth);
                    children.push(child);
                }

                let mut state = access.state;
                if let (Some(t), Some(f)) = (taffy_id, self.focused) {
                    if t == f {
                        state.focused = true;
                    }
                }

                // Affordances are derived from role + state (disabled / read-only
                // gate the verbs; focus does not), so compute them once against
                // the focus-updated node.
                let actions = verbs_for(&AccessNode {
                    state,
                    ..access.clone()
                });

                let node = SemanticNode {
                    id: id.clone(),
                    role: access.role,
                    name: access.name.clone(),
                    value: if access.state.protected { None } else { access.value.clone() },
                    state,
                    bounds: Some(w.get_rect()),
                    actions,
                    children,
                    widget_taffy_id: taffy_id.map(u64::from),
                };
                self.order.push(id.clone());
                self.nodes.insert(id.clone(), node);
                parent_children.push(id);
            }
            None => {
                for child in w.children() {
                    self.visit_widget(child, parent_children);
                }
            }
        }
    }

    /// Emit a synthetic a11y child (a table row/cell) that has no backing widget.
    fn visit_synthetic(&mut self, parent: &SemanticId, index: usize, access: &AccessNode) -> SemanticId {
        let id = SemanticId::synthetic(parent, index);
        let mut children: Vec<SemanticId> = Vec::new();
        for (i, child) in access.children.iter().enumerate() {
            let c = self.visit_synthetic(&id, i, child);
            children.push(c);
        }
        let node = SemanticNode {
            id: id.clone(),
            role: access.role,
            name: access.name.clone(),
            value: if access.state.protected { None } else { access.value.clone() },
            state: access.state,
            bounds: None,
            actions: verbs_for(access),
            children,
            widget_taffy_id: None,
        };
        self.order.push(id.clone());
        self.nodes.insert(id.clone(), node);
        id
    }

    /// Emit a resource node and its exported actions, recursing into child
    /// resources. Returns the node's id.
    fn visit_resource(&mut self, res: &Resource) -> SemanticId {
        let id = SemanticId::resource(res.id.as_str());

        let mut actions: Vec<SemanticAction> = Vec::new();
        for spec in &res.actions {
            actions.push(self.resource_action(&id, spec));
        }

        let mut children: Vec<SemanticId> = Vec::new();
        for child in &res.children {
            children.push(self.visit_resource(child));
        }

        let node = SemanticNode {
            id: id.clone(),
            role: Role::Group,
            name: Some(res.label.clone()),
            value: None,
            state: AccessState::default(),
            bounds: None,
            actions,
            children,
            widget_taffy_id: None,
        };
        self.order.push(id.clone());
        self.nodes.insert(id.clone(), node);
        id
    }

    fn resource_action(&mut self, node_id: &SemanticId, spec: &ActionSpec) -> SemanticAction {
        let kind = match &spec.input {
            ActionInput::Immediate(enc) => SemanticActionKind::App(enc.clone()),
            ActionInput::Prompted { prompt, placeholder, collects, build } => {
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
    use crate::widget::{Button, Column, Widget};
    use std::collections::HashMap;
    use taffy::TaffyTree;

    /// Lay a widget tree out the way the App does, populating the `node_map` the
    /// tree keys ids off, and return `(node_map, focused-first-focusable)`.
    fn layout(root: &mut dyn Widget) -> HashMap<taffy::NodeId, *mut dyn Widget> {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let node = root.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(node, taffy::Size { width: taffy::AvailableSpace::Definite(800.0), height: taffy::AvailableSpace::Definite(600.0) })
            .unwrap();
        root.apply_layout(&taffy, node, vello::kurbo::Point::ZERO);
        node_map
    }

    #[test]
    fn identity_is_stable_across_frames() {
        let mut col = Column::new()
            .push(Box::new(Button::new("a", "Alpha", None)))
            .push(Box::new(Button::new("b", "Beta", None)));
        let node_map = layout(&mut col);

        let mut tree = SemanticTree::new();
        let first = tree.refresh(&col, &node_map, None, &[]);
        // First frame: the two buttons are added (the root pre-exists from
        // `new()`, so it registers as updated, not added, once it gains children).
        assert!(first.removed.is_empty());
        assert!(first.added.len() >= 2, "the two buttons are added, got {:?}", first.added);
        let ids_first: Vec<SemanticId> = tree.iter().map(|n| n.id.clone()).collect();
        assert!(ids_first.len() >= 3, "root + two buttons, got {ids_first:?}");
        // The buttons carry stable widget ids (data-pointer keyed off the taffy
        // node map), not synthetic fallbacks.
        assert!(
            ids_first.iter().filter(|id| id.as_str().starts_with('w')).count() >= 2,
            "buttons get taffy-derived widget ids, got {ids_first:?}"
        );

        // Second frame, nothing changed: identical ids, empty delta.
        let second = tree.refresh(&col, &node_map, None, &[]);
        assert!(second.is_empty(), "a stable frame yields no delta, got {second:?}");
        let ids_second: Vec<SemanticId> = tree.iter().map(|n| n.id.clone()).collect();
        assert_eq!(ids_first, ids_second, "node identity + order survive a refresh");
    }

    #[test]
    fn focus_change_updates_the_same_node() {
        let mut col = Column::new()
            .push(Box::new(Button::new("a", "Alpha", None)))
            .push(Box::new(Button::new("b", "Beta", None)));
        let node_map = layout(&mut col);
        // The taffy id of a focusable button, so we can mark it focused.
        let first_button_taffy = node_map
            .iter()
            .find_map(|(t, &w)| {
                let focusable = unsafe { (*w).accessibility() }
                    .map(|a| a.state.focusable)
                    .unwrap_or(false);
                focusable.then_some(*t)
            })
            .expect("a focusable button exists");

        let mut tree = SemanticTree::new();
        tree.refresh(&col, &node_map, None, &[]);
        let focused_id = SemanticId::widget(first_button_taffy);
        let before = tree.node(&focused_id).expect("button node present").clone();
        assert!(!before.state.focused, "not focused initially");

        // Focus that button; the SAME node id must now report focused, and show
        // up as `updated` (not added/removed — identity preserved).
        let delta = tree.refresh(&col, &node_map, Some(first_button_taffy), &[]);
        let after = tree.node(&focused_id).expect("button node still present");
        assert!(after.state.focused, "focus state reflected on the retained node");
        assert_eq!(before.id, after.id, "identity preserved across the state change");
        assert!(delta.updated.contains(&focused_id), "the focused node is in the delta, got {delta:?}");
        assert!(delta.added.is_empty() && delta.removed.is_empty(), "only state changed");
    }

    #[test]
    fn added_and_removed_nodes_surface_in_the_delta() {
        let mut two = Column::new()
            .push(Box::new(Button::new("a", "Alpha", None)))
            .push(Box::new(Button::new("b", "Beta", None)));
        let map2 = layout(&mut two);
        let mut tree = SemanticTree::new();
        tree.refresh(&two, &map2, None, &[]);
        let baseline: std::collections::HashSet<SemanticId> = tree.iter().map(|n| n.id.clone()).collect();

        // A smaller tree: one button removed.
        let mut one = Column::new().push(Box::new(Button::new("a", "Alpha", None)));
        let map1 = layout(&mut one);
        let delta = tree.refresh(&one, &map1, None, &[]);
        assert!(!delta.removed.is_empty(), "dropping a button reports a removal, got {delta:?}");
        let now: std::collections::HashSet<SemanticId> = tree.iter().map(|n| n.id.clone()).collect();
        assert!(now.len() < baseline.len(), "the tree shrank");
    }

    #[test]
    fn button_node_affords_focus_and_activate_verbs() {
        let mut btn = Button::new("go", "Go", None);
        let node_map = layout(&mut btn);
        let mut tree = SemanticTree::new();
        tree.refresh(&btn, &node_map, None, &[]);
        let node = tree.iter().find(|n| n.name.as_deref() == Some("Go")).expect("button node");
        assert!(node.action("activate").is_some(), "a button affords activate");
        assert!(node.action("focus").is_some(), "a focusable button affords focus");
        assert!(matches!(node.action("activate").unwrap().kind, SemanticActionKind::Verb(Action::Activate)));
    }
}
