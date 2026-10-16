//! The semantic nodes of one bound component's published resources, kept
//! between refreshes and updated from what changed in its resource tree.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use tilo_actions::{ActionInput, ActionSpec, Resource};

use super::{
    AccessState, PromptedMap, Role, SemanticAction, SemanticActionKind, SemanticId, SemanticNode,
    TreeDelta,
};
use crate::bound_dispatch::{
    BoundActionAddress, BoundActionTarget, BoundResourceSnapshot, CallbackBinding,
};
use crate::published::{Change, SharedChildren, SharedResource};

pub(super) type BoundActions = HashMap<(SemanticId, String), BoundActionTarget>;

/// What a component's nodes were built from.
enum Source {
    /// A shared forest, kept so the next one is diffed against it.
    Shared(Rc<[SharedResource]>),
    /// An ordinary tree, reused while the component hands over the same one.
    Plain(Weak<Vec<Resource>>),
}

/// The semantic nodes of one bound component's published resources, and the
/// tree they were built from. Nothing else goes into them: an id, its actions
/// and their bound targets come from the resource tree and the component's
/// identity alone. So while the component hands over the same tree they are
/// exactly what a rebuild would produce, and a tree that shares all
/// but a few subtrees with the last changes the nodes of those subtrees only.
pub(super) struct ComponentNodes {
    source: Source,
    binding: CallbackBinding,
    /// `ri:<instance>:<lifetime>:`, what every node id here starts with.
    prefix: String,
    roots: Vec<SemanticId>,
    pub(super) nodes: HashMap<SemanticId, SemanticNode>,
    /// Reading order (each node after its children), derived on first use
    /// after a change.
    order: OnceCell<Vec<SemanticId>>,
    pub(super) prompted: PromptedMap,
    pub(super) bound_actions: BoundActions,
    pub(super) duplicate_ids: Vec<SemanticId>,
    /// The nodes built when these were built, or last updated.
    #[cfg(any(test, feature = "test-support"))]
    built: usize,
}

impl ComponentNodes {
    /// The nodes of `component`'s resources, built afresh.
    pub(super) fn build(component: &BoundResourceSnapshot) -> Self {
        let binding = component.binding.clone();
        let mut nodes = Self {
            source: Source::Plain(Weak::new()),
            prefix: id_prefix(&binding),
            binding,
            roots: Vec::new(),
            nodes: HashMap::new(),
            order: OnceCell::new(),
            prompted: HashMap::new(),
            bound_actions: HashMap::new(),
            duplicate_ids: Vec::new(),
            #[cfg(any(test, feature = "test-support"))]
            built: 0,
        };
        let mut order = Vec::new();
        if let Some(forest) = component.shared_forest() {
            for resource in forest.iter() {
                let root = nodes.insert_shared(resource, &mut order);
                nodes.roots.push(root);
            }
            nodes.source = Source::Shared(Rc::clone(forest));
        } else {
            let resources = component.resources();
            for resource in resources.iter() {
                let root = nodes.insert_plain(resource, &mut order);
                nodes.roots.push(root);
            }
            nodes.source = Source::Plain(Rc::downgrade(resources));
        }
        nodes.order = OnceCell::from(order);
        nodes
    }

    /// Whether these are the nodes of `component`'s owner, in the same
    /// captured lifetime: the only nodes `component`'s may be updated from.
    pub(super) fn same_owner(&self, component: &BoundResourceSnapshot) -> bool {
        self.binding.same_component(&component.binding)
            && self.binding.instance_id == component.binding.instance_id
            && self.binding.lifetime_id == component.binding.lifetime_id
    }

    /// Whether these nodes are exactly what `component` would build.
    pub(super) fn built_from(&self, component: &BoundResourceSnapshot) -> bool {
        self.same_owner(component)
            && match (&self.source, component.shared_forest()) {
                (Source::Shared(source), Some(forest)) => Rc::ptr_eq(source, forest),
                (Source::Plain(source), None) => source
                    .upgrade()
                    .is_some_and(|source| Rc::ptr_eq(&source, component.resources())),
                _ => false,
            }
    }

    pub(super) fn roots(&self) -> &[SemanticId] {
        &self.roots
    }

    /// The nodes built when these were built, or last updated.
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn built_for_test(&self) -> usize {
        self.built
    }

    /// The nodes in reading order.
    pub(super) fn order(&self) -> &[SemanticId] {
        self.order.get_or_init(|| {
            fn after_children(
                id: &SemanticId,
                nodes: &HashMap<SemanticId, SemanticNode>,
                order: &mut Vec<SemanticId>,
            ) {
                if let Some(node) = nodes.get(id) {
                    for child in &node.children {
                        after_children(child, nodes, order);
                    }
                }
                order.push(id.clone());
            }
            let mut order = Vec::with_capacity(self.nodes.len());
            for root in &self.roots {
                after_children(root, &self.nodes, &mut order);
            }
            order
        })
    }

    /// Move these nodes to `component`'s shared forest, visiting only what
    /// differs from the forest they mirror, and answer what changed. `None`
    /// leaves them as they were: they mirror no shared forest, or a node id
    /// would repeat, which only a rebuild reports as the full walk does.
    pub(super) fn update(&mut self, component: &BoundResourceSnapshot) -> Option<TreeDelta> {
        let Source::Shared(last) = &self.source else {
            return None;
        };
        let next = component.shared_forest()?;
        if !self.duplicate_ids.is_empty() {
            return None;
        }
        let last = Rc::clone(last);
        let mut removed = Vec::new();
        let mut added = Vec::new();
        let mut heads = Vec::new();
        let mut roots_changed = false;
        crate::published::diff(&last, next, &mut |change| match change {
            Change::Removed(subtree) => removed.push(subtree),
            Change::Added(subtree) => added.push(subtree),
            Change::Head { last, next } => heads.push((last, next)),
            Change::Roots => roots_changed = true,
        });
        // Check everything before changing anything, so a refusal leaves
        // the nodes as they were.
        let mut removed_ids = Vec::new();
        for subtree in &removed {
            visit_subtree(subtree, &mut |node| removed_ids.push(self.id_of(node)));
        }
        let gone: HashSet<&SemanticId> = removed_ids.iter().collect();
        if gone.len() != removed_ids.len()
            || removed_ids.iter().any(|id| !self.nodes.contains_key(id))
        {
            return None;
        }
        let mut added_ids = Vec::new();
        for subtree in &added {
            visit_subtree(subtree, &mut |node| added_ids.push(self.id_of(node)));
        }
        let mut seen = HashSet::with_capacity(added_ids.len());
        if added_ids
            .iter()
            .any(|id| !seen.insert(id) || (self.nodes.contains_key(id) && !gone.contains(id)))
        {
            return None;
        }
        let head_ids = heads
            .iter()
            .map(|(_, next)| self.id_of(next))
            .collect::<Vec<_>>();
        if head_ids
            .iter()
            .any(|id| gone.contains(id) || seen.contains(id) || !self.nodes.contains_key(id))
        {
            return None;
        }
        drop((gone, seen));
        // The nodes built from here on carry the binding the component
        // hands over now, as a rebuild's would.
        self.binding = component.binding.clone();
        #[cfg(any(test, feature = "test-support"))]
        {
            self.built = 0;
        }

        let mut delta = TreeDelta::default();
        let mut removed_nodes = HashMap::with_capacity(removed_ids.len());
        for id in removed_ids {
            if let Some(node) = self.remove_node(&id) {
                removed_nodes.insert(id, node);
            }
        }
        for ((last, next), id) in heads.into_iter().zip(head_ids) {
            // A head republished with what its node shows unchanged (a
            // record's root, whose actions are minted afresh with every
            // revision while a keystroke changes its body) keeps its node
            // and its actions' targets; only its children may move.
            if self.keeps_node_of(&id, next.head()) {
                if self.keep_head(&id, last, next) {
                    delta.updated.push(id);
                }
                continue;
            }
            let Some(mut old) = self.remove_node(&id) else {
                continue;
            };
            let old_children = std::mem::take(&mut old.children);
            let mut node = self.resource_node(id.clone(), next.head(), Vec::new());
            let mut same = node.same_exported_content(&old);
            node.children = if last.children().ptr_eq(next.children())
                || self.same_child_ids(&old_children, next.children())
            {
                old_children
            } else {
                let children = next
                    .children()
                    .iter()
                    .map(|child| self.id_of(child))
                    .collect::<Vec<_>>();
                same &= children == old_children;
                children
            };
            if !same {
                delta.updated.push(id.clone());
            }
            self.nodes.insert(id, node);
        }
        let mut order = Vec::new();
        for subtree in added {
            self.insert_shared(subtree, &mut order);
        }
        if roots_changed {
            self.roots = next.iter().map(|root| self.id_of(root)).collect();
        }
        for id in added_ids {
            match removed_nodes.remove(&id) {
                Some(old) => {
                    if self
                        .nodes
                        .get(&id)
                        .is_none_or(|node| !node.same_exported_content(&old))
                    {
                        delta.updated.push(id);
                    }
                }
                None => delta.added.push(id),
            }
        }
        delta.removed.extend(removed_nodes.into_keys());
        self.source = Source::Shared(Rc::clone(next));
        self.order = OnceCell::new();
        Some(delta)
    }

    fn id_of(&self, resource: &SharedResource) -> SemanticId {
        let id = resource.id();
        let mut text = String::with_capacity(self.prefix.len().saturating_add(id.len()));
        text.push_str(&self.prefix);
        text.push_str(id);
        SemanticId(text)
    }

    /// Whether `ids` are the ids of `children`, in order, compared without
    /// building them.
    fn same_child_ids(&self, ids: &[SemanticId], children: &SharedChildren) -> bool {
        ids.len() == children.len()
            && ids.iter().zip(children).all(|(id, child)| {
                let child = child.id();
                id.0.len() == self.prefix.len().saturating_add(child.len())
                    && id.0.starts_with(&self.prefix)
                    && id.0.ends_with(child)
            })
    }

    /// Whether node `id` is the node `head` builds but for its children:
    /// the same label and the same actions, compared without building them.
    /// Its actions' bound targets are then the ones a rebuild records too:
    /// they come from the resource's id, each action's id and the
    /// component's identity alone.
    fn keeps_node_of(&self, id: &SemanticId, head: &Resource) -> bool {
        let Some(node) = self.nodes.get(id) else {
            return false;
        };
        let kept = node.name.as_deref() == Some(head.label.as_str())
            && node.actions.len() == head.actions.len()
            && node
                .actions
                .iter()
                .zip(&head.actions)
                .all(|(action, spec)| exports_spec(action, spec));
        #[cfg(debug_assertions)]
        if kept {
            let mut prompted = HashMap::new();
            let actions = resource_actions(id, head, None, &mut prompted, None);
            let fresh =
                resource_semantic_node(id.clone(), head, actions, node.children.clone(), false);
            debug_assert!(
                fresh == *node,
                "resource node {id:?} kept with a head that builds another node"
            );
        }
        kept
    }

    /// Keep node `id` for `next`, whose head builds it ([`Self::keeps_node_of`]),
    /// with `next`'s children and its prompted actions' builders, as a
    /// rebuild records them. Returns whether its children changed.
    fn keep_head(&mut self, id: &SemanticId, last: &SharedResource, next: &SharedResource) -> bool {
        for spec in &next.head().actions {
            if let ActionInput::Prompted { build, .. } = &spec.input {
                self.prompted
                    .insert((id.clone(), spec.id.clone()), build.clone());
            }
        }
        if last.children().ptr_eq(next.children()) {
            return false;
        }
        let Some(old_children) = self.nodes.get(id).map(|node| &node.children) else {
            return false;
        };
        if self.same_child_ids(old_children, next.children()) {
            return false;
        }
        let children = next
            .children()
            .iter()
            .map(|child| self.id_of(child))
            .collect::<Vec<_>>();
        let Some(node) = self.nodes.get_mut(id) else {
            return false;
        };
        let changed = node.children != children;
        node.children = children;
        changed
    }

    /// Remove node `id` and its actions' targets.
    fn remove_node(&mut self, id: &SemanticId) -> Option<SemanticNode> {
        let node = self.nodes.remove(id)?;
        for action in &node.actions {
            let key = (id.clone(), action.name.clone());
            self.prompted.remove(&key);
            self.bound_actions.remove(&key);
        }
        Some(node)
    }

    /// `head`'s node with `children`, its actions' targets recorded.
    fn resource_node(
        &mut self,
        id: SemanticId,
        head: &Resource,
        children: Vec<SemanticId>,
    ) -> SemanticNode {
        #[cfg(any(test, feature = "test-support"))]
        {
            self.built = self.built.saturating_add(1);
        }
        let actions = resource_actions(
            &id,
            head,
            Some(&self.binding),
            &mut self.prompted,
            Some(&mut self.bound_actions),
        );
        resource_semantic_node(id, head, actions, children, false)
    }

    fn insert_node(&mut self, node: SemanticNode, order: &mut Vec<SemanticId>) -> SemanticId {
        let id = node.id.clone();
        order.push(id.clone());
        if self.nodes.insert(id.clone(), node).is_some() {
            self.duplicate_ids.push(id.clone());
        }
        id
    }

    fn insert_shared(
        &mut self,
        resource: &SharedResource,
        order: &mut Vec<SemanticId>,
    ) -> SemanticId {
        let id = self.id_of(resource);
        #[cfg(any(test, feature = "test-support"))]
        {
            self.built = self.built.saturating_add(1);
        }
        // Actions before children, as the full walk records them.
        let actions = resource_actions(
            &id,
            resource.head(),
            Some(&self.binding),
            &mut self.prompted,
            Some(&mut self.bound_actions),
        );
        let children = resource
            .children()
            .iter()
            .map(|child| self.insert_shared(child, order))
            .collect();
        let node = resource_semantic_node(id, resource.head(), actions, children, false);
        self.insert_node(node, order)
    }

    fn insert_plain(&mut self, resource: &Resource, order: &mut Vec<SemanticId>) -> SemanticId {
        let id = bound_resource_id(&self.binding, resource.id.as_str());
        #[cfg(any(test, feature = "test-support"))]
        {
            self.built = self.built.saturating_add(1);
        }
        let actions = resource_actions(
            &id,
            resource,
            Some(&self.binding),
            &mut self.prompted,
            Some(&mut self.bound_actions),
        );
        let children = resource
            .children
            .iter()
            .map(|child| self.insert_plain(child, order))
            .collect();
        let node = resource_semantic_node(id, resource, actions, children, false);
        self.insert_node(node, order)
    }
}

/// `node` and every node beneath it, each after its children.
fn visit_subtree(node: &SharedResource, visit: &mut dyn FnMut(&SharedResource)) {
    for child in node.children() {
        visit_subtree(child, visit);
    }
    visit(node);
}

fn id_prefix(binding: &CallbackBinding) -> String {
    format!("ri:{}:{}:", binding.instance_id, binding.lifetime_id)
}

/// The node id of resource `id` published by `binding`'s component.
pub(super) fn bound_resource_id(binding: &CallbackBinding, id: &str) -> SemanticId {
    SemanticId(format!(
        "ri:{}:{}:{}",
        binding.instance_id, binding.lifetime_id, id
    ))
}

/// The affordances of `resource`'s own actions on node `id`, recording each
/// prompted builder and, for a component's resource, each bound target.
pub(super) fn resource_actions(
    id: &SemanticId,
    resource: &Resource,
    binding: Option<&CallbackBinding>,
    prompted: &mut PromptedMap,
    mut bound_actions: Option<&mut BoundActions>,
) -> Vec<SemanticAction> {
    let mut actions = Vec::with_capacity(resource.actions.len());
    for spec in &resource.actions {
        actions.push(resource_action(id, resource.id.as_str(), spec, prompted));
        if let (Some(binding), Some(bound_actions)) = (binding, bound_actions.as_deref_mut()) {
            bound_actions.insert(
                (id.clone(), spec.id.clone()),
                BoundActionTarget {
                    input: crate::bound_dispatch::Input::Discrete,
                    binding: binding.clone(),
                    address: BoundActionAddress {
                        instance_id: binding.instance_id,
                        address: tilo_actions::ActionAddress::new(resource.id.as_str(), &spec.id),
                    },
                },
            );
        }
    }
    actions
}

/// The node of `resource` with `actions` and `children`. Only declared
/// resources (no component) keep their identity beyond this lifetime.
pub(super) fn resource_semantic_node(
    id: SemanticId,
    resource: &Resource,
    actions: Vec<SemanticAction>,
    children: Vec<SemanticId>,
    durable_identity: bool,
) -> SemanticNode {
    SemanticNode {
        id,
        durable_identity,
        role: Role::Group,
        name: Some(resource.label.clone()),
        value: None,
        state: AccessState::default(),
        bounds: None,
        actions,
        children,
        widget_taffy_id: None,
        semantic_target: None,
    }
}

/// Whether `action` is the affordance `spec` exports on its resource's
/// node ([`resource_action`]): the same id, label and input. A deferred
/// action's address is its resource's id and its own, both the same here.
fn exports_spec(action: &SemanticAction, spec: &ActionSpec) -> bool {
    let same_input = match (&action.kind, &spec.input) {
        (SemanticActionKind::App(encoded), ActionInput::Immediate(next)) => {
            encoded == next.as_ref()
        }
        (SemanticActionKind::AppDeferred { .. }, ActionInput::Deferred { .. }) => true,
        (
            SemanticActionKind::AppPrompted {
                prompt,
                placeholder,
                collects,
            },
            ActionInput::Prompted {
                prompt: next_prompt,
                placeholder: next_placeholder,
                collects: next_collects,
                ..
            },
        ) => prompt == next_prompt && placeholder == next_placeholder && collects == next_collects,
        _ => false,
    };
    // The action is named by the spec's id.
    let named = action.name == spec.id;
    same_input && named && action.label == spec.label
}

fn resource_action(
    node_id: &SemanticId,
    resource: &str,
    spec: &ActionSpec,
    prompted: &mut PromptedMap,
) -> SemanticAction {
    let kind = match &spec.input {
        ActionInput::Immediate(enc) => SemanticActionKind::App(enc.as_ref().clone()),
        ActionInput::Deferred { .. } => SemanticActionKind::AppDeferred {
            address: tilo_actions::ActionAddress::new(resource, &spec.id),
        },
        ActionInput::Prompted {
            prompt,
            placeholder,
            collects,
            build,
        } => {
            prompted.insert((node_id.clone(), spec.id.clone()), build.clone());
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
