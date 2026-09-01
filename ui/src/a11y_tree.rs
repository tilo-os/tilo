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
//! [`Widget::accessibility`](crate::widget::Widget::accessibility) walk
//! (roles/names/values/state, collapsing presentational widgets like the
//! AccessKit bridge does) contributes generic verbs (focus, activate, …);
//! the app's [`resources()`](crate::widget::Widget::resources) forest from
//! the qOS [action model](qos_actions) contributes
//! [`SemanticActionKind::App`] affordances carrying the exact
//! [`EncodedAction`](qos_actions::EncodedAction) a real click would send —
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

use crate::a11y::{AccessNode, AccessState, Action, Role};
use crate::widget::Widget;

type HostedWidget<'a> = (Vec<usize>, &'a dyn Widget);
type HostedByIndex<'a> = HashMap<usize, Vec<HostedWidget<'a>>>;
use qos_actions::{ActionInput, ActionSpec, Collects, EncodedAction, OperationKind, Resource};
use serde::{Deserialize, Serialize};

/// Operation type used when the retained semantic tree projects an ordinary
/// UI affordance into the resource/action catalogue. This makes the same
/// control addressable from Ctrl+K, the automation socket, and agents without
/// inventing an app-specific command bus.
pub(crate) const SEMANTIC_OPERATION_TYPE: &str = "qos.ui.semantic-action";

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
                semantic_target: None,
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
    /// `node_map` is used *read-only* (never dereferenced) to assign each
    /// widget its stable id, exactly as
    /// [`accesskit_bridge`](crate::accesskit_bridge) does. `focused` folds
    /// into each node's [`AccessState::focused`]. `resources` is the focused
    /// app's open-resource forest
    /// ([`Widget::resources`](crate::widget::Widget::resources)); each
    /// exported [`ActionSpec`](qos_actions::ActionSpec) becomes an invokable
    /// affordance.
    pub fn refresh(
        &mut self,
        root: &dyn Widget,
        node_map: &HashMap<TaffyNodeId, *mut dyn Widget>,
        focused: Option<TaffyNodeId>,
        resources: &[Resource],
    ) -> TreeDelta {
        // Reverse map widget *data* pointer -> taffy id, keyed on the thin
        // data address (`*const ()`) rather than the fat `*const dyn Widget`
        // — the data address of a live object is identical across every
        // coercion of it, so the lookup is immune to vtable-pointer
        // differences between the `self`-coercion `node_map` stored during
        // `build_layout` and the `&dyn Widget` a parent hands back from
        // `children()`.
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

        let mut root_children: Vec<SemanticId> = Vec::new();
        builder.visit_widget(root, &mut root_children);

        // Resource affordances hang off the root as their own subtrees.
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
                semantic_target: None,
            },
        );

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

    pub fn root(&self) -> &SemanticId {
        &self.root
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

    /// The first node whose accessible [`name`](SemanticNode::name) equals
    /// `name`, in reading order — the ergonomic handle for a CLI / agent that
    /// addresses elements by label ("focus the node called 'Volume'").
    pub fn find_by_name(&self, name: &str) -> Option<&SemanticNode> {
        self.iter().find(|n| n.name.as_deref() == Some(name))
    }

    pub fn action(&self, id: &SemanticId, name: &str) -> Option<&SemanticAction> {
        self.nodes.get(id).and_then(|n| n.action(name))
    }

    /// Project every currently operable accessibility node into the canonical
    /// qOS resource/action surface. Bespoke app actions remain preferable for
    /// domain operations; this projection closes the driveability gap for
    /// ordinary controls such as buttons, fields, tabs, toggles, and sliders.
    pub(crate) fn action_resources(&self) -> Vec<Resource> {
        self.iter()
            .filter(|node| node.id != self.root && !node.id.0.starts_with("r:"))
            .filter_map(|node| {
                let verbs = node
                    .actions
                    .iter()
                    .filter_map(|action| match &action.kind {
                        SemanticActionKind::Verb(verb) => Some((action, verb)),
                        SemanticActionKind::App(_) | SemanticActionKind::AppPrompted { .. } => None,
                    })
                    .collect::<Vec<_>>();
                if verbs.is_empty() {
                    return None;
                }

                let label = node
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", node.role));
                let mut resource =
                    Resource::new(format!("ui:{}", node.id.as_str()), "qos.ui.control", label);
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
                            OperationKind::Effect,
                            move |value| {
                                let operation = SemanticOperation {
                                    node: operation.node.clone(),
                                    action: operation.action.clone(),
                                    value: Some(value.to_string()),
                                };
                                EncodedAction::encode_operation(SEMANTIC_OPERATION_TYPE, &operation)
                                    .expect("semantic operation is serializable")
                            },
                        ),
                        _ => match ActionSpec::operation(
                            action.name.clone(),
                            action.label.clone(),
                            OperationKind::Effect,
                            SEMANTIC_OPERATION_TYPE,
                            &operation,
                        ) {
                            Ok(spec) => spec,
                            Err(_) => continue,
                        },
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

/// Separate from [`SemanticTree`] so the retained frame is only replaced
/// once the new one is fully built (and diffed against).
struct Build<'a> {
    nodes: HashMap<SemanticId, SemanticNode>,
    order: Vec<SemanticId>,
    prompted: PromptedMap,
    ptr_to_taffy: &'a HashMap<*const (), TaffyNodeId>,
    focused: Option<TaffyNodeId>,
}

impl Build<'_> {
    /// A presentational widget (`accessibility() == None`) is collapsed
    /// out and its children reparented — matching the AccessKit bridge.
    fn visit_widget(&mut self, w: &dyn Widget, parent_children: &mut Vec<SemanticId>) {
        let taffy_id = self
            .ptr_to_taffy
            .get(&std::ptr::from_ref::<dyn Widget>(w).cast::<()>())
            .copied();
        match w.accessibility() {
            Some(access) => {
                let id = if let Some(t) = taffy_id {
                    SemanticId::widget(t)
                } else {
                    // Rare (never laid out); key under the parent so it
                    // still gets a stable id.
                    let root = SemanticId::root();
                    let parent = parent_children.last().unwrap_or(&root);
                    SemanticId::synthetic(parent, parent_children.len())
                };

                let mut children: Vec<SemanticId> = Vec::new();
                let mut hosted = HostedByIndex::new();
                for (child_index, child) in w.children().into_iter().enumerate() {
                    if let Some(mut path) = w.accessibility_child_path(child_index)
                        && !path.is_empty()
                    {
                        let parent_index = path.remove(0);
                        hosted.entry(parent_index).or_default().push((path, child));
                    } else {
                        self.visit_widget(child, &mut children);
                    }
                }
                for (i, synth) in access.children.iter().enumerate() {
                    let child = self.visit_synthetic(
                        &id,
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
                let actions = verbs_for(&AccessNode {
                    state,
                    ..access.clone()
                });

                let node = SemanticNode {
                    id: id.clone(),
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
                    semantic_target: None,
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

    /// A virtual row/cell — has no backing widget, but may carry geometry.
    fn visit_synthetic(
        &mut self,
        parent: &SemanticId,
        index: usize,
        access: &AccessNode,
        owner: Option<TaffyNodeId>,
        hosted: Vec<HostedWidget<'_>>,
    ) -> SemanticId {
        let id = SemanticId::synthetic(parent, index);
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
                    i,
                    child,
                    owner,
                    nested.remove(&i).unwrap_or_default(),
                );
                children.push(c);
            }
        } else {
            for child in direct {
                self.visit_widget(child, &mut children);
            }
        }
        let node = SemanticNode {
            id: id.clone(),
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
        self.nodes.insert(id.clone(), node);
        id
    }

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
            semantic_target: None,
        };
        self.order.push(id.clone());
        self.nodes.insert(id.clone(), node);
        id
    }

    fn resource_action(&mut self, node_id: &SemanticId, spec: &ActionSpec) -> SemanticAction {
        let kind = match &spec.input {
            ActionInput::Immediate(enc) => SemanticActionKind::App(enc.clone()),
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
    fn identity_is_stable_across_frames() {
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
            ids_first
                .iter()
                .filter(|id| id.as_str().starts_with('w'))
                .count()
                >= 2,
            "buttons get taffy-derived widget ids, got {ids_first:?}"
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

    #[test]
    fn focus_change_updates_the_same_node() {
        let mut col = Column::new()
            .push(Box::new(Button::new("a", "Alpha", None)))
            .push(Box::new(Button::new("b", "Beta", None)));
        let node_map = layout(&mut col);
        let first_button_taffy = node_map
            .iter()
            .find_map(|(t, &w)| {
                let focusable = unsafe { (*w).accessibility() }.is_some_and(|a| a.state.focusable);
                focusable.then_some(*t)
            })
            .expect("a focusable button exists");

        let mut tree = SemanticTree::new();
        tree.refresh(&col, &node_map, None, &[]);
        let focused_id = SemanticId::widget(first_button_taffy);
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

    #[test]
    fn added_and_removed_nodes_surface_in_the_delta() {
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

    #[test]
    fn button_node_affords_focus_and_activate_verbs() {
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

    #[test]
    fn plain_text_is_not_falsely_advertised_as_activatable() {
        let label = crate::a11y::AccessNode::new(Role::Label);
        assert!(
            verbs_for(&label)
                .iter()
                .all(|action| action.name != "activate"),
            "the retained semantic tree must agree with AccessKit"
        );
    }

    #[test]
    fn operable_controls_project_into_resource_actions() {
        let mut button = Button::new("go", "Go", None);
        let node_map = layout(&mut button);
        let mut tree = SemanticTree::new();
        tree.refresh(&button, &node_map, None, &[]);
        let resources = tree.action_resources();
        let go = resources
            .iter()
            .find(|resource| resource.label == "Go")
            .expect("button is externally driveable");
        assert_eq!(go.kind, "qos.ui.control");
        assert!(go.actions.iter().any(|action| action.id == "activate"));
    }

    #[test]
    fn projected_text_value_reaches_the_semantic_operation() {
        let mut field = TextInput::new(Vec::new()).with_accessible_name("Destination");
        let node_map = layout(&mut field);
        let mut tree = SemanticTree::new();
        tree.refresh(&field, &node_map, None, &[]);
        let resources = tree.action_resources();
        let spec = resources
            .iter()
            .find(|resource| resource.label == "Destination")
            .and_then(|resource| {
                resource
                    .actions
                    .iter()
                    .find(|action| action.id == "set-value")
            })
            .expect("editable field publishes set-value");
        let encoded = match &spec.input {
            ActionInput::Prompted { build, .. } => build("/tmp/report.pdf"),
            ActionInput::Immediate(_) => panic!("set-value must collect a value"),
        };
        let operation: SemanticOperation = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(operation.value.as_deref(), Some("/tmp/report.pdf"));
    }

    #[test]
    fn retained_table_control_is_nested_under_its_synthetic_cell() {
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
