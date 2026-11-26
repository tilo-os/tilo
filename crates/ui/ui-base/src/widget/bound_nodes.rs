//! Admission of live widget/Taffy nodes to their record owners, and the checked entry points that
//! deliver a callback to one admitted node.
use super::Widget;
use super::component_walk::{Binding, ComponentScope, ScopeEntry, WalkError, binding_of};
use crate::accesskit_bridge::A11yTarget;
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};

/// The component scope a node is admitted under, which the nodes below it inherit.
type Scope = Option<Binding>;

/// What entering a node's scope holds open until it drops.
type EntryScope = Option<ScopeEntry>;

/// Whether `scope` is a component's.
fn scoped(scope: &Scope) -> bool {
    scope.is_some()
}

/// Enter `scope` for one visit to a node admitted under it.
fn enter_scope(scope: &Scope) -> Result<EntryScope, WalkError> {
    scope.as_deref().map(ComponentScope::enter).transpose()
}

/// The scope a root is admitted under: the component it is, if it is one.
fn root_scope(root: &dyn Widget) -> Scope {
    binding_of(root)
}

/// The scope `widget` is admitted under, below a node admitted under `inherited`.
fn widget_scope(widget: &dyn Widget, inherited: Scope) -> Scope {
    if widget.resets_binding_inheritance() {
        None
    } else {
        binding_of(widget).or(inherited)
    }
}

/// One admitted node: the live widget at it, and the component it answers to.
pub struct BoundNode {
    pub pointer: *mut dyn Widget,
    pub semantic_name: String,
    pub binding: Scope,
    pub inactive: bool,
}

/// What entering a widget for a walk ([`enter_widget`]) holds open until it drops: with the model,
/// the scope of the component the widget is.
pub struct NodeEntry {
    _scope: EntryScope,
}

/// Admission metadata for precisely one live widget/Taffy layout.
#[derive(Default)]
pub struct BoundNodeMap {
    pub nodes: HashMap<NodeId, BoundNode>,
    pub(crate) parents: HashMap<NodeId, NodeId>,
}
type PendingAdmission = (NodeId, Scope, bool);

#[derive(Default)]
struct Boundaries {
    inactive: std::collections::HashSet<NodeId>,
}

impl Boundaries {
    fn collect(&mut self, widget: &dyn Widget) {
        if let Some(node) = widget.inactive_bound_dispatch_boundary() {
            self.inactive.insert(node);
        }
        for child in widget.children() {
            self.collect(child);
        }
    }
}

impl BoundNodeMap {
    /// Current admitted layout ancestry, including structural non-widget nodes.
    pub(crate) fn is_descendant(&self, mut node: NodeId, ancestor: NodeId) -> bool {
        while let Some(parent) = self.parents.get(&node) {
            if *parent == ancestor {
                return true;
            }
            node = *parent;
        }
        false
    }
    pub fn binding_for(&self, node: NodeId) -> Option<Binding> {
        self.nodes
            .get(&node)
            .filter(|node| !node.inactive)
            .and_then(|node| node.binding.clone())
    }
    /// [`Self::binding_for`] without copying the binding, for a walk that asks it of every node.
    pub fn binding_ref(&self, node: NodeId) -> Option<&Binding> {
        self.nodes
            .get(&node)
            .filter(|node| !node.inactive)
            .and_then(|node| node.binding.as_ref())
    }
    /// Whether `node` was admitted inside a bound component.
    pub fn has_owner(&self, node: NodeId) -> bool {
        self.nodes
            .get(&node)
            .is_some_and(|entry| scoped(&entry.binding))
    }
    pub fn owns_document(&self, node: NodeId) -> bool {
        self.nodes
            .get(&node)
            .is_some_and(|entry| scoped(&entry.binding))
    }
    pub fn new() -> Self {
        Self::default()
    }
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.parents.clear();
    }
    /// Admit raw nodes using their nearest boundary in the matching layout topology.
    ///
    /// # Safety
    /// Root, tree, layout root and pointers must belong to the same live build under
    /// one exclusive tree owner. No replacement, rebuild or destruction may occur.
    pub unsafe fn rebuild(
        &mut self,
        root: &dyn Widget,
        taffy: &TaffyTree<()>,
        layout_root: NodeId,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        // SAFETY: the public contract covers the single ownership/presentation root.
        unsafe {
            self.rebuild_with_retained_root(root, None, taffy, layout_root, nodes);
        }
    }

    /// Collect ownership from a separately retained live root while inheriting
    /// bindings only through the presentation layout. Chrome stays unbound.
    /// # Safety
    /// Both roots and every admitted pointer belong to the same exclusively
    /// owned settled composition; neither root may be replaced during this call.
    pub(crate) unsafe fn rebuild_with_retained_root(
        &mut self,
        root: &dyn Widget,
        retained: Option<&dyn Widget>,
        taffy: &TaffyTree<()>,
        layout_root: NodeId,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        // SAFETY: the caller owns all supplied roots and layout pointers.
        unsafe { self.rebuild_roots_with_retained(&[(root, layout_root)], retained, taffy, nodes) };
    }

    /// Admit disjoint presentation roots without inheriting authority across roots.
    /// # Safety
    /// Every root and node must belong to the same exclusively owned live arena.
    pub unsafe fn rebuild_roots(
        &mut self,
        roots: &[(&dyn Widget, NodeId)],
        taffy: &TaffyTree<()>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        // SAFETY: the caller supplies the shared live arena and disjoint subtrees.
        unsafe { self.rebuild_roots_with_retained(roots, None, taffy, nodes) };
    }

    pub unsafe fn rebuild_roots_with_retained(
        &mut self,
        roots: &[(&dyn Widget, NodeId)],
        retained: Option<&dyn Widget>,
        taffy: &TaffyTree<()>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        self.clear();
        let mut boundaries = Boundaries::default();
        for (root, _) in roots {
            boundaries.collect(*root);
        }
        if let Some(retained) = retained {
            boundaries.collect(retained);
        }
        let pending: Vec<_> = roots
            .iter()
            .map(|(root, layout_root)| (*layout_root, root_scope(*root), false))
            .collect();
        // SAFETY: the caller owns all supplied roots and layout pointers.
        unsafe { self.admit(pending, &boundaries, taffy, nodes) };
    }

    /// Admit the subtree at `child`, which the widget at `parent` mounted in place
    /// ([`crate::widget::Remount`]), as a rebuild would: under the binding and inactivity `parent` was
    /// admitted with, and the boundaries the subtree widgets declare.
    pub unsafe fn admit_mounted(
        &mut self,
        parent: NodeId,
        child: NodeId,
        taffy: &TaffyTree<()>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) -> bool {
        let Some(entry) = self.nodes.get(&parent) else {
            return false;
        };
        let inherited = (entry.binding.clone(), entry.inactive);
        let Some(&pointer) = nodes.get(&child) else {
            return false;
        };
        let mut boundaries = Boundaries::default();
        // SAFETY: the caller guarantees every admitted pointer is live.
        boundaries.collect(unsafe { &*pointer });
        // The nodes between `child` and `parent` that are no widget's (a part's wrapper) are its
        // ancestry too.
        let mut at = child;
        loop {
            let up = taffy.parent(at).unwrap_or(parent);
            self.parents.insert(at, up);
            if up == parent || self.nodes.contains_key(&up) {
                break;
            }
            at = up;
        }
        let (binding, inactive) = inherited;
        // SAFETY: forwarded from the caller.
        unsafe {
            self.admit(vec![(child, binding, inactive)], &boundaries, taffy, nodes);
        }
        true
    }

    /// Forget `removed`, nodes a widget unmounted in place ([`crate::widget::Remount`]).
    pub fn forget(&mut self, removed: &[NodeId]) {
        for node in removed {
            self.nodes.remove(node);
            self.parents.remove(node);
        }
    }

    unsafe fn admit(
        &mut self,
        mut pending: Vec<PendingAdmission>,
        boundaries: &Boundaries,
        taffy: &TaffyTree<()>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        while let Some((node, inherited, inherited_inactive)) = pending.pop() {
            let inactive = inherited_inactive || boundaries.inactive.contains(&node);
            let binding = if let Some(&pointer) = nodes.get(&node) {
                // SAFETY: rebuild's owner guarantees all admitted pointers are live.
                let widget = unsafe { &*pointer };
                let binding = widget_scope(widget, inherited);
                self.nodes.insert(
                    node,
                    BoundNode {
                        pointer,
                        semantic_name: widget.name().to_owned(),
                        binding: binding.clone(),
                        inactive,
                    },
                );
                binding
            } else {
                inherited
            };
            if let Ok(children) = taffy.children(node) {
                for child in children {
                    self.parents.insert(child, node);
                    pending.push((child, binding.clone(), inactive));
                }
            }
        }
    }
}

/// Enter a retained boundary for the complete duration of a tree walk.
pub fn enter_widget(widget: &dyn Widget) -> Result<NodeEntry, WalkError> {
    let scope = binding_of(widget)
        .map(|binding| binding.enter())
        .transpose()?;
    Ok(NodeEntry { _scope: scope })
}

impl BoundNodeMap {
    pub fn admitted(
        &self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        node: NodeId,
    ) -> Result<&BoundNode, WalkError> {
        let admitted = self.nodes.get(&node).ok_or(WalkError::MissingNode)?;
        let pointer = nodes.get(&node).ok_or(WalkError::MissingNode)?;
        if !std::ptr::eq(admitted.pointer, *pointer) {
            return Err(WalkError::MissingNode);
        }
        Ok(admitted)
    }
}

/// Shared inspections never borrow the widget's canonical document.
/// # Safety
/// Same live-tree and stable-topology requirements as `invoke_bound_node`;
/// only shared target references may overlap and none may escape.
pub unsafe fn inspect_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    callback: impl FnOnce(&dyn Widget) -> R,
) -> Result<R, WalkError> {
    let admitted = bindings.admitted(nodes, node)?;
    let _entry = enter_scope(&admitted.binding)?;
    // SAFETY: admission and lifetime checked; caller owns the stable live tree.
    Ok(callback(unsafe { &*admitted.pointer }))
}

/// Measure or settle one node's geometry.
pub(crate) unsafe fn layout_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    callback: impl FnOnce(&mut dyn Widget) -> R,
) -> Result<R, WalkError> {
    let admitted = bindings.admitted(nodes, node)?;
    let _entry = enter_scope(&admitted.binding)?;
    // SAFETY: admission and lifetime checked; caller exclusively owns this target.
    Ok(callback(unsafe { &mut *admitted.pointer }))
}

/// One walk over many admitted nodes, asking each a question that reads or settles only the widget
/// itself (a flag, a measurement, a layout style).
pub struct NodeSweep<'m> {
    bindings: &'m BoundNodeMap,
    nodes: &'m HashMap<NodeId, *mut dyn Widget>,
    held: Option<(&'m dyn ComponentScope, ScopeEntry)>,
}

impl<'m> NodeSweep<'m> {
    pub fn new(bindings: &'m BoundNodeMap, nodes: &'m HashMap<NodeId, *mut dyn Widget>) -> Self {
        Self {
            bindings,
            nodes,
            held: None,
        }
    }

    /// Enter `binding`'s scope unless the sweep already has the same one.
    fn enter(&mut self, binding: Option<&'m dyn ComponentScope>) -> Result<(), WalkError> {
        let Some(binding) = binding else {
            self.held = None;
            return Ok(());
        };
        if let Some((held, _)) = &self.held
            && (std::ptr::addr_eq(*held, binding) || held.enters_same_scope(binding))
        {
            binding.check_open()?;
            return Ok(());
        }
        self.held = None;
        let guard = binding.enter()?;
        self.held = Some((binding, guard));
        Ok(())
    }

    /// [`inspect_bound_node`] within this sweep.
    pub unsafe fn inspect<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&dyn Widget) -> R,
    ) -> Result<R, WalkError> {
        let admitted = self.bindings.admitted(self.nodes, node)?;
        self.enter(admitted.binding.as_deref())?;
        // SAFETY: admission and lifetime checked; caller owns the stable live tree.
        Ok(callback(unsafe { &*admitted.pointer }))
    }

    /// [`layout_bound_node`] within this sweep.
    pub(crate) unsafe fn layout<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&mut dyn Widget) -> R,
    ) -> Result<R, WalkError> {
        let admitted = self.bindings.admitted(self.nodes, node)?;
        self.enter(admitted.binding.as_deref())?;
        // SAFETY: admission and lifetime checked; caller exclusively owns this target.
        Ok(callback(unsafe { &mut *admitted.pointer }))
    }
}

/// Non-document mutations (focus/measurement) enter the same lifetime.
pub unsafe fn mutate_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    callback: impl FnOnce(&mut dyn Widget) -> R,
) -> Result<R, WalkError> {
    let admitted = bindings.admitted(nodes, node)?;
    if admitted.inactive {
        return Err(WalkError::MissingNode);
    }
    let _entry = enter_scope(&admitted.binding)?;
    // SAFETY: admission and lifetime checked; caller exclusively owns this target.
    Ok(callback(unsafe { &mut *admitted.pointer }))
}

/// Explicit compatibility mode for retained helpers; component adapters still
/// enforce their own entry when called by an unbound legacy traversal.
pub fn enter_traversal<const BOUND: bool>(widget: &dyn Widget) -> Result<NodeEntry, WalkError> {
    if BOUND {
        enter_widget(widget)
    } else {
        Ok(NodeEntry { _scope: None })
    }
}

/// The names of the admitted nodes the accessibility bridge may address.
#[derive(Default)]
pub struct AdmittedNodeNames {
    names: HashMap<NodeId, (*mut dyn Widget, String)>,
}
impl AdmittedNodeNames {
    pub fn insert(&mut self, node: NodeId, pointer: *mut dyn Widget, name: String) {
        self.names.insert(node, (pointer, name));
    }
    pub fn get(&self, node: NodeId, pointer: *mut dyn Widget) -> Option<&str> {
        let (admitted, name) = self.names.get(&node)?;
        std::ptr::eq(*admitted, pointer).then_some(name)
    }
}

/// The live, admitted nodes exported to the AT bridge.
pub struct ExportedNodes {
    pub nodes: HashMap<NodeId, *mut dyn Widget>,
    pub names: AdmittedNodeNames,
}
impl ExportedNodes {
    pub fn focus(&self, node: Option<NodeId>) -> Option<NodeId> {
        node.filter(|node| self.nodes.contains_key(node))
    }
    pub fn resolve(&self, root: &dyn Widget, id: accesskit::NodeId) -> Option<A11yTarget> {
        let target = crate::accesskit_bridge::resolve_node_id_bound(root, &self.nodes, id)?;
        let exported = |node: &NodeId| self.nodes.contains_key(node);
        match &target {
            A11yTarget::Root => {}
            A11yTarget::Widget(node) | A11yTarget::Synthetic { owner: node, .. } => {
                if !exported(node) {
                    return None;
                }
            }
        }
        Some(target)
    }
}
impl BoundNodeMap {
    pub fn semantic_node_names(
        &self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) -> AdmittedNodeNames {
        let mut names = AdmittedNodeNames::default();
        for (&node, &pointer) in nodes {
            let Some(admitted) = self.nodes.get(&node) else {
                continue;
            };
            if !std::ptr::eq(admitted.pointer, pointer) || admitted.inactive {
                continue;
            }
            names.insert(node, pointer, admitted.semantic_name.clone());
        }
        names
    }
    pub fn exported_nodes(&self, nodes: &HashMap<NodeId, *mut dyn Widget>) -> ExportedNodes {
        let mut exported = ExportedNodes {
            nodes: HashMap::new(),
            names: AdmittedNodeNames::default(),
        };
        for (&node, &pointer) in nodes {
            let Some(admitted) = self.nodes.get(&node) else {
                continue;
            };
            if !std::ptr::eq(admitted.pointer, pointer) || admitted.inactive {
                continue;
            }
            exported.nodes.insert(node, pointer);
            exported
                .names
                .insert(node, pointer, admitted.semantic_name.clone());
        }
        exported
    }
}
