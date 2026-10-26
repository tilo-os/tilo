//! Admission of live widget/Taffy nodes to their record owners, and the
//! checked entry points that deliver a callback to one admitted node.
use super::{
    BoundDispatchError, BoundNode, CallbackBinding, CallbackScopeGuard, DocumentEditStamp,
    a11y_owner,
};
use crate::permissions::UserId;
use crate::widget::Widget;
use automerge::{AutoCommit, ObjId, ROOT};
use std::collections::HashMap;
use std::rc::Rc;
use taffy::{NodeId, TaffyTree};

/// Admission metadata for precisely one live widget/Taffy layout.
#[derive(Default)]
pub struct BoundNodeMap {
    pub(super) nodes: HashMap<NodeId, BoundNode>,
    pub(super) parents: HashMap<NodeId, NodeId>,
    // Persistent across layout clears; aliases are allocated only for a new
    // owner/lifetime + local node, never once per redraw or tree rebuild.
    pub(super) a11y_aliases: a11y_owner::OwnerAliases,
}
/// A node to admit, with the binding, legacy owner and inactivity it inherits.
type PendingAdmission = (
    NodeId,
    Option<Rc<CallbackBinding>>,
    Option<crate::history_mode::HistoryOwner>,
    bool,
);

/// The legacy-owner and inactive callback boundaries widgets declare.
#[derive(Default)]
struct Boundaries {
    legacy: HashMap<NodeId, crate::history_mode::HistoryOwner>,
    inactive: std::collections::HashSet<NodeId>,
}

impl Boundaries {
    /// Add the boundaries `widget` and its descendants declare.
    fn collect(&mut self, widget: &dyn Widget) {
        if let Some(node) = widget.inactive_callback_boundary() {
            self.inactive.insert(node);
        }
        if let Some((node, owner)) = widget.legacy_callback_boundary() {
            self.legacy.insert(node, owner);
        }
        for child in widget.children() {
            self.collect(child);
        }
    }
}

impl BoundNodeMap {
    /// Read the nearest canonical record owner's document and history stamp.
    /// Local writes, remote merges and undo/redo invalidate it, including an
    /// edit followed by its inverse. Reading does not open an action or commit.
    /// # Errors
    /// Rejects missing/inactive nodes, retired owners, borrowed records and
    /// exhausted generations; callers must cancel deferred text insertion.
    pub fn document_edit_stamp(
        &self,
        node: NodeId,
        document: &AutoCommit,
    ) -> Result<DocumentEditStamp, BoundDispatchError> {
        let entry = self
            .nodes
            .get(&node)
            .filter(|entry| !entry.inactive)
            .ok_or(BoundDispatchError::MissingNode)?;
        if let Some(binding) = &entry.binding {
            binding.document_edit_stamp(document)
        } else if let Some(legacy) = &entry.legacy {
            legacy
                .validate()
                .map_err(|_| BoundDispatchError::ClosedLifetime)?;
            let (store, checkout) = legacy.store_checkout();
            let lifetime = legacy
                .lifetime_id()
                .ok_or(BoundDispatchError::ClosedLifetime)?;
            store
                .document_edit_stamp(&checkout.id, lifetime)
                .map_err(BoundDispatchError::Record)
        } else {
            Ok(DocumentEditStamp::for_document(document))
        }
    }
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
    pub(crate) fn binding_for(&self, node: NodeId) -> Option<CallbackBinding> {
        self.nodes
            .get(&node)
            .filter(|node| !node.inactive)
            .and_then(|node| node.binding.as_deref().cloned())
    }
    /// [`Self::binding_for`] without copying the binding, for a walk that
    /// asks it of every node.
    pub(crate) fn binding_ref(&self, node: NodeId) -> Option<&CallbackBinding> {
        self.nodes
            .get(&node)
            .filter(|node| !node.inactive)
            .and_then(|node| node.binding.as_deref())
    }
    /// Whether `node` was admitted inside a bound component or a legacy tile.
    pub(crate) fn has_owner(&self, node: NodeId) -> bool {
        self.nodes
            .get(&node)
            .is_some_and(|entry| entry.binding.is_some() || entry.legacy.is_some())
    }
    pub(crate) fn legacy_for(&self, node: NodeId) -> Option<crate::history_mode::HistoryOwner> {
        self.nodes.get(&node).and_then(|entry| entry.legacy.clone())
    }
    /// Whether any admitted node belongs to a legacy tile, whose record no
    /// [`bound_owner_standing`] read follows.
    pub(crate) fn has_legacy(&self) -> bool {
        self.nodes.values().any(|entry| entry.legacy.is_some())
    }
    pub(crate) fn legacy_status(
        &self,
        node: NodeId,
        author: &UserId,
    ) -> Option<crate::undo::UndoStatus> {
        let owner = self.legacy_for(node)?;
        owner.validate().ok()?;
        let (store, checkout) = owner.store_checkout();
        // A status read is not a mutable loan: it must not advance the
        // record's action generation.
        store.read_history(&checkout.id, |doc, history| {
            crate::undo::status_snapshot(doc, history, Some(author))
        })
    }
    pub(crate) fn owns_document(&self, node: NodeId) -> bool {
        self.nodes
            .get(&node)
            .is_some_and(|entry| entry.binding.is_some() || entry.legacy.is_some())
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

    /// Collect custody from a separately retained live root while inheriting
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

    pub(crate) unsafe fn rebuild_roots_with_retained(
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
            .map(|(root, layout_root)| {
                (
                    *layout_root,
                    root.callback_binding().map(Rc::new),
                    None,
                    false,
                )
            })
            .collect();
        // SAFETY: the caller owns all supplied roots and layout pointers.
        unsafe { self.admit(pending, &boundaries, taffy, nodes) };
    }

    /// Admit the subtree at `child`, which the widget at `parent` mounted in
    /// place ([`crate::widget::Remount`]), as a rebuild would: under the
    /// binding, legacy owner and inactivity `parent` was admitted with, and
    /// the boundaries the subtree's own widgets declare. Returns `false`
    /// when `parent` or `child` is not admitted, and the host rebuilds.
    ///
    /// # Safety
    /// As [`Self::rebuild`]: the tree, the maps and every pointer belong to
    /// one live build under one exclusive owner.
    pub(crate) unsafe fn admit_mounted(
        &mut self,
        parent: NodeId,
        child: NodeId,
        taffy: &TaffyTree<()>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) -> bool {
        let Some(entry) = self.nodes.get(&parent) else {
            return false;
        };
        let inherited = (entry.binding.clone(), entry.legacy.clone(), entry.inactive);
        let Some(&pointer) = nodes.get(&child) else {
            return false;
        };
        let mut boundaries = Boundaries::default();
        // SAFETY: the caller guarantees every admitted pointer is live.
        boundaries.collect(unsafe { &*pointer });
        // Its layout parent may be a structural node under `parent`.
        self.parents
            .insert(child, taffy.parent(child).unwrap_or(parent));
        let (binding, legacy, inactive) = inherited;
        // SAFETY: forwarded from the caller.
        unsafe {
            self.admit(
                vec![(child, binding, legacy, inactive)],
                &boundaries,
                taffy,
                nodes,
            );
        }
        true
    }

    /// Forget `removed`, nodes a widget unmounted in place
    /// ([`crate::widget::Remount`]).
    pub(crate) fn forget(&mut self, removed: &[NodeId]) {
        for node in removed {
            self.nodes.remove(node);
            self.parents.remove(node);
        }
    }

    /// Admit each pending node and everything under it, inheriting its
    /// binding, legacy owner and inactivity down the layout tree.
    unsafe fn admit(
        &mut self,
        mut pending: Vec<PendingAdmission>,
        boundaries: &Boundaries,
        taffy: &TaffyTree<()>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
    ) {
        while let Some((node, inherited, inherited_legacy, inherited_inactive)) = pending.pop() {
            let inactive = inherited_inactive || boundaries.inactive.contains(&node);
            let mut legacy = boundaries.legacy.get(&node).cloned().or(inherited_legacy);
            let binding = if let Some(&pointer) = nodes.get(&node) {
                // SAFETY: rebuild's owner guarantees all admitted pointers are live.
                let widget = unsafe { &*pointer };
                let binding = if widget.resets_callback_binding() {
                    None
                } else {
                    widget.callback_binding().map(Rc::new).or(inherited)
                };
                if binding.is_some() || widget.resets_callback_binding() {
                    legacy = None;
                }
                self.nodes.insert(
                    node,
                    BoundNode {
                        pointer,
                        semantic_name: widget.name().to_owned(),
                        binding: binding.clone(),
                        legacy: legacy.clone(),
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
                    pending.push((child, binding.clone(), legacy.clone(), inactive));
                }
            }
        }
        self.a11y_aliases.admit(&self.nodes);
    }
}

/// Invoke an admitted node under its original component lifetime and record.
///
/// # Errors
/// Refuses missing, replaced, closed or synchronously cross-record targets.
///
/// # Safety
/// For admitted open or explicitly unbound delivery, the caller exclusively owns
/// the matching live tree/topology with no overlapping target or ancestor reference.
/// No replacement, rebuild or destruction may occur during delivery; no widget
/// reference or callback may escape. Closed entries may retain their actual old
/// pointers after destruction: metadata and lifetime are checked before dereference.
#[allow(
    clippy::implicit_hasher,
    reason = "shared producer/consumer contract uses the existing default-hasher node map"
)]
pub unsafe fn invoke_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    outer: &mut AutoCommit,
    outer_root: &ObjId,
    callback: impl FnOnce(&mut dyn Widget, &mut AutoCommit, &ObjId) -> R,
) -> Result<R, BoundDispatchError> {
    let admitted = bindings
        .nodes
        .get(&node)
        .ok_or(BoundDispatchError::MissingNode)?;
    let pointer = nodes.get(&node).ok_or(BoundDispatchError::MissingNode)?;
    if !std::ptr::eq(admitted.pointer, *pointer) {
        return Err(BoundDispatchError::MissingNode);
    }
    if admitted.inactive {
        return Err(BoundDispatchError::MissingNode);
    }
    if let Some(binding) = &admitted.binding {
        binding.with_document(outer, |document, root| {
            // SAFETY: admission and lifetime were checked before this dereference;
            // the caller guarantees live topology and exclusive widget access.
            let widget = unsafe { &mut **pointer };
            let result = callback(widget, document, root);
            let _ = binding.drain_owned_tree(widget, document);
            result
        })
    } else if let Some(owner) = &admitted.legacy {
        owner
            .validate()
            .map_err(|_| BoundDispatchError::ClosedLifetime)?;
        let (store, checkout) = owner.store_checkout();
        store
            .with_checked(&checkout.id, |doc| {
                callback(unsafe { &mut **pointer }, doc, &ROOT)
            })
            .map_err(BoundDispatchError::Record)
    } else {
        // Explicitly admitted unbound widgets preserve the outer document.
        Ok(callback(unsafe { &mut **pointer }, outer, outer_root))
    }
}

/// Enter a retained boundary for the complete duration of a tree walk.
pub(crate) fn enter_widget(
    widget: &dyn Widget,
) -> Result<Option<CallbackScopeGuard>, BoundDispatchError> {
    widget
        .callback_binding()
        .map(|binding| binding.enter())
        .transpose()
}

impl BoundNodeMap {
    pub(super) fn admitted(
        &self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        node: NodeId,
    ) -> Result<&BoundNode, BoundDispatchError> {
        let admitted = self
            .nodes
            .get(&node)
            .ok_or(BoundDispatchError::MissingNode)?;
        let pointer = nodes.get(&node).ok_or(BoundDispatchError::MissingNode)?;
        if !std::ptr::eq(admitted.pointer, *pointer) {
            return Err(BoundDispatchError::MissingNode);
        }
        if let Some(owner) = &admitted.legacy {
            owner
                .validate()
                .map_err(|_| BoundDispatchError::ClosedLifetime)?;
        }
        Ok(admitted)
    }
}

/// Shared inspections never borrow the widget's canonical document.
/// # Safety
/// Same live-tree and stable-topology requirements as `invoke_bound_node`;
/// only shared target references may overlap and none may escape.
pub(crate) unsafe fn inspect_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    callback: impl FnOnce(&dyn Widget) -> R,
) -> Result<R, BoundDispatchError> {
    let admitted = bindings.admitted(nodes, node)?;
    let _entry = admitted
        .binding
        .as_deref()
        .map(CallbackBinding::enter)
        .transpose()?;
    // SAFETY: admission and lifetime checked; caller owns the stable live tree.
    Ok(callback(unsafe { &*admitted.pointer }))
}

/// Measure or settle one node's geometry. History leaves the live app on
/// screen beside its timeline, inert but visible, so layout still reaches
/// it: only input, focus and effects stop at an inactive boundary.
/// # Safety
/// Same exclusive target/tree requirements as `invoke_bound_node`.
pub(crate) unsafe fn layout_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    callback: impl FnOnce(&mut dyn Widget) -> R,
) -> Result<R, BoundDispatchError> {
    let admitted = bindings.admitted(nodes, node)?;
    let _entry = admitted
        .binding
        .as_deref()
        .map(CallbackBinding::enter)
        .transpose()?;
    // SAFETY: admission and lifetime checked; caller exclusively owns this target.
    Ok(callback(unsafe { &mut *admitted.pointer }))
}

/// One walk over many admitted nodes, asking each a question that reads or
/// settles only the widget itself (a flag, a measurement, a layout style).
///
/// Every node of a component enters that component's callback scope
/// ([`CallbackBinding::enter`]), which sets the executing host's identity and
/// copies three strings into it. A walk of a long list entered it once per
/// widget, thousands of times a frame, to answer one flag each. A sweep
/// keeps the scope it entered while the following nodes enter the same one
/// ([`CallbackBinding::enters_same_scope`]), as a tree walk does under
/// [`enter_widget`], and leaves it before a node of another component, a
/// legacy tile or an unbound node, so each widget still answers inside its
/// own scope and nowhere else. The record and lifetime are checked for
/// every node, as entering checks them.
pub(crate) struct NodeSweep<'m> {
    bindings: &'m BoundNodeMap,
    nodes: &'m HashMap<NodeId, *mut dyn Widget>,
    held: Option<(&'m CallbackBinding, CallbackScopeGuard)>,
}

impl<'m> NodeSweep<'m> {
    pub(crate) fn new(
        bindings: &'m BoundNodeMap,
        nodes: &'m HashMap<NodeId, *mut dyn Widget>,
    ) -> Self {
        Self {
            bindings,
            nodes,
            held: None,
        }
    }

    /// Enter `binding`'s scope unless the sweep already holds the same one.
    fn enter(&mut self, binding: Option<&'m CallbackBinding>) -> Result<(), BoundDispatchError> {
        let Some(binding) = binding else {
            self.held = None;
            return Ok(());
        };
        if let Some((held, _)) = &self.held
            && (std::ptr::eq(*held, binding) || held.enters_same_scope(binding))
        {
            binding.check_record()?;
            return Ok(());
        }
        // The scope held is left before the next is entered: each restores
        // what was there before it.
        self.held = None;
        let guard = binding.enter()?;
        self.held = Some((binding, guard));
        Ok(())
    }

    /// Leave the scope the sweep holds, before code that enters scopes of
    /// its own runs between two of its nodes.
    pub(crate) fn leave(&mut self) {
        self.held = None;
    }

    /// [`inspect_bound_node`] within this sweep.
    ///
    /// # Safety
    /// As [`inspect_bound_node`].
    pub(crate) unsafe fn inspect<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&dyn Widget) -> R,
    ) -> Result<R, BoundDispatchError> {
        let admitted = self.bindings.admitted(self.nodes, node)?;
        self.enter(admitted.binding.as_deref())?;
        // SAFETY: admission and lifetime checked; caller owns the stable live tree.
        Ok(callback(unsafe { &*admitted.pointer }))
    }

    /// [`layout_bound_node`] within this sweep.
    ///
    /// # Safety
    /// As [`layout_bound_node`].
    pub(crate) unsafe fn layout<R>(
        &mut self,
        node: NodeId,
        callback: impl FnOnce(&mut dyn Widget) -> R,
    ) -> Result<R, BoundDispatchError> {
        let admitted = self.bindings.admitted(self.nodes, node)?;
        self.enter(admitted.binding.as_deref())?;
        // SAFETY: admission and lifetime checked; caller exclusively owns this target.
        Ok(callback(unsafe { &mut *admitted.pointer }))
    }
}

/// Non-document mutations (focus/measurement) enter the same lifetime.
/// # Safety
/// Same exclusive target/tree requirements as `invoke_bound_node`.
pub(crate) unsafe fn mutate_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    callback: impl FnOnce(&mut dyn Widget) -> R,
) -> Result<R, BoundDispatchError> {
    let admitted = bindings.admitted(nodes, node)?;
    if admitted.inactive {
        return Err(BoundDispatchError::MissingNode);
    }
    let _entry = admitted
        .binding
        .as_deref()
        .map(CallbackBinding::enter)
        .transpose()?;
    // SAFETY: admission and lifetime checked; caller exclusively owns this target.
    Ok(callback(unsafe { &mut *admitted.pointer }))
}

/// Explicit compatibility mode for retained helpers; component adapters still
/// enforce their own entry when called by an unbound legacy traversal.
pub(crate) fn enter_traversal<const BOUND: bool>(
    widget: &dyn Widget,
) -> Result<Option<CallbackScopeGuard>, BoundDispatchError> {
    if BOUND {
        enter_widget(widget)
    } else {
        Ok(None)
    }
}
