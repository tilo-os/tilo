//! Lifetime and canonical-record routing shared by retained and raw widget dispatch.
use crate::permissions::{PermissionContext, UserId};
use crate::record::{RecordCheckout, RecordOpenError, RecordStore};
use crate::wake::{WakeHandle, WakeScopeGuard};
use crate::widget::Widget;
use automerge::{AutoCommit, ObjId, ROOT};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use taffy::{NodeId, TaffyTree};

/// Runtime metadata; does not own the component's widget or mount lifetime.
#[derive(Clone)]
pub struct CallbackBinding {
    pub(crate) lifetime: WakeHandle,
    pub(crate) closing: Rc<Cell<bool>>,
    #[allow(
        dead_code,
        reason = "binding retains original authority for the following typed-action provenance task"
    )]
    pub(crate) instance_id: crate::application::InstanceId,
    pub(crate) records: RecordStore,
    pub(crate) checkout: RecordCheckout,
    pub(crate) permissions: Rc<RefCell<PermissionContext>>,
    #[allow(
        dead_code,
        reason = "binding retains original authority for the following typed-action provenance task"
    )]
    pub(crate) author: UserId,
    #[allow(
        dead_code,
        reason = "binding retains original authority for the following typed-action provenance task"
    )]
    pub(crate) operation_context: qos_actions::ActionContext,
}

#[derive(Debug)]
pub enum BoundDispatchError {
    MissingNode,
    ClosedLifetime,
    CrossRecordReentry,
    Record(RecordOpenError),
}

thread_local! {
    static ACTIVE: RefCell<Option<(RecordStore, RecordCheckout, *const AutoCommit)>> = const { RefCell::new(None) };
}
struct ActiveEntry;
impl Drop for ActiveEntry {
    fn drop(&mut self) {
        ACTIVE.with_borrow_mut(|active| *active = None);
    }
}
impl CallbackBinding {
    fn check_record(&self) -> Result<Option<*const AutoCommit>, BoundDispatchError> {
        if self.closing.get() || !self.lifetime.is_active() {
            return Err(BoundDispatchError::ClosedLifetime);
        }
        ACTIVE.with_borrow(|active| match active {
            Some((records, checkout, document)) => {
                if self.records.same_owner(records) && self.checkout == *checkout {
                    Ok(Some(*document))
                } else {
                    Err(BoundDispatchError::CrossRecordReentry)
                }
            }
            None => Ok(None),
        })
    }
    pub(crate) fn enter(&self) -> Result<WakeScopeGuard, BoundDispatchError> {
        self.check_record()?;
        self.lifetime
            .enter_if_open()
            .map_err(|_| BoundDispatchError::ClosedLifetime)
    }
    pub(crate) fn with_document<R>(
        &self,
        outer: &mut AutoCommit,
        callback: impl FnOnce(&mut AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        let active = self.check_record()?;
        let _scope = self.enter()?;
        if let Some(document) = active {
            if !std::ptr::eq(document, outer) {
                // Reentry must carry the currently borrowed document, never reconstruct
                // a mutable reference from a saved pointer.
                return Err(BoundDispatchError::Record(RecordOpenError::Busy));
            }
            return Ok(callback(outer, &ROOT));
        }
        self.records
            .with_checked(&self.checkout.id, |document| {
                ACTIVE.with_borrow_mut(|active| {
                    *active = Some((self.records.clone(), self.checkout.clone(), document));
                });
                let _active = ActiveEntry;
                callback(document, &ROOT)
            })
            .map_err(BoundDispatchError::Record)
    }
    pub(crate) fn read_document<R>(
        &self,
        outer: &AutoCommit,
        callback: impl FnOnce(&AutoCommit, &ObjId) -> R,
    ) -> Result<R, BoundDispatchError> {
        let active = self.check_record()?;
        let _scope = self.enter()?;
        if let Some(document) = active {
            if !std::ptr::eq(document, outer) {
                return Err(BoundDispatchError::Record(RecordOpenError::Busy));
            }
            return Ok(callback(outer, &ROOT));
        }
        self.records
            .with_checked(&self.checkout.id, |document| {
                ACTIVE.with_borrow_mut(|active| {
                    *active = Some((self.records.clone(), self.checkout.clone(), document));
                });
                let _active = ActiveEntry;
                callback(document, &ROOT)
            })
            .map_err(BoundDispatchError::Record)
    }
}

struct BoundNode {
    pointer: *mut dyn Widget,
    binding: Option<CallbackBinding>,
}
/// Admission metadata for precisely one live widget/Taffy layout.
#[derive(Default)]
pub struct BoundNodeMap {
    nodes: HashMap<NodeId, BoundNode>,
}
impl BoundNodeMap {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn clear(&mut self) {
        self.nodes.clear();
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
        self.clear();
        let mut pending = vec![(layout_root, root.callback_binding())];
        while let Some((node, inherited)) = pending.pop() {
            let binding = if let Some(&pointer) = nodes.get(&node) {
                // SAFETY: rebuild's owner guarantees all admitted pointers are live.
                let binding = unsafe { (&*pointer).callback_binding() }.or(inherited);
                self.nodes.insert(
                    node,
                    BoundNode {
                        pointer,
                        binding: binding.clone(),
                    },
                );
                binding
            } else {
                inherited
            };
            if let Ok(children) = taffy.children(node) {
                pending.extend(children.into_iter().map(|child| (child, binding.clone())));
            }
        }
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
    if let Some(binding) = &admitted.binding {
        binding.with_document(outer, |document, root| {
            // SAFETY: admission and lifetime were checked before this dereference;
            // the caller guarantees live topology and exclusive widget access.
            callback(unsafe { &mut **pointer }, document, root)
        })
    } else {
        // Explicitly admitted unbound widgets preserve the outer document.
        Ok(callback(unsafe { &mut **pointer }, outer, outer_root))
    }
}

/// Enter a retained boundary for the complete duration of a tree walk.
pub(crate) fn enter_widget(
    widget: &dyn Widget,
) -> Result<Option<WakeScopeGuard>, BoundDispatchError> {
    widget
        .callback_binding()
        .map(|binding| binding.enter())
        .transpose()
}

impl BoundNodeMap {
    fn admitted(
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
        .as_ref()
        .map(CallbackBinding::enter)
        .transpose()?;
    // SAFETY: admission and lifetime checked; caller owns the stable live tree.
    Ok(callback(unsafe { &*admitted.pointer }))
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
    let _entry = admitted
        .binding
        .as_ref()
        .map(CallbackBinding::enter)
        .transpose()?;
    // SAFETY: admission and lifetime checked; caller exclusively owns this target.
    Ok(callback(unsafe { &mut *admitted.pointer }))
}

/// Explicit compatibility mode for retained helpers; component adapters still
/// enforce their own entry when called by an unbound legacy traversal.
pub(crate) fn enter_traversal<const BOUND: bool>(
    widget: &dyn Widget,
) -> Result<Option<WakeScopeGuard>, BoundDispatchError> {
    if BOUND {
        enter_widget(widget)
    } else {
        Ok(None)
    }
}
