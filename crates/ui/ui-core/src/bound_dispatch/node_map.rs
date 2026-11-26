//! The entry points that deliver a callback to one admitted node under the
//! record its component holds, and the component scope a callback binding is
//! to the toolkit's walks ([`ComponentScope`]).
use super::BoundDispatchError;
#[cfg(feature = "model")]
use super::{CallbackBinding, CallbackScopeGuard, DocumentEditStamp};
use tilo_ui_base::widget::Widget;
use tilo_ui_base::widget::bound_nodes::BoundNodeMap;
use tilo_ui_base::widget::component_walk::WalkError;
#[cfg(feature = "model")]
use tilo_ui_base::{widget::component_walk::ComponentScope, widget::component_walk::ScopeEntry};
use automerge::{AutoCommit, ObjId};
#[cfg(feature = "model")]
use std::cell::RefCell;
use std::collections::HashMap;
use taffy::NodeId;

impl From<WalkError> for BoundDispatchError {
    fn from(error: WalkError) -> Self {
        match error {
            WalkError::MissingNode => Self::MissingNode,
            WalkError::ClosedLifetime => Self::ClosedLifetime,
            WalkError::CrossResourceReentry => Self::CrossResourceReentry,
        }
    }
}

/// The callback binding a component scope of this crate's walks is: the app
/// layer admits no other.
#[cfg(feature = "model")]
#[expect(
    clippy::expect_used,
    reason = "the app layer implements component scopes for callback bindings alone"
)]
pub(crate) fn callback_binding_of(scope: &dyn ComponentScope) -> &CallbackBinding {
    (scope as &dyn std::any::Any)
        .downcast_ref()
        .expect("every component scope a widget answers is a callback binding")
}

/// Entering a scope fails only on its lifetime or an open record.
#[cfg(feature = "model")]
#[expect(
    clippy::unreachable,
    reason = "entering a callback binding refuses only a closed lifetime or another open record"
)]
fn walk_error(error: BoundDispatchError) -> WalkError {
    match error {
        BoundDispatchError::MissingNode => WalkError::MissingNode,
        BoundDispatchError::ClosedLifetime => WalkError::ClosedLifetime,
        BoundDispatchError::CrossResourceReentry => WalkError::CrossResourceReentry,
        other => unreachable!("entering a component scope failed on {other:?}"),
    }
}

#[cfg(feature = "model")]
thread_local! {
    /// The scope guards the walks' [`ScopeEntry`]s hold, each in the slot its entry names. Entries
    /// drop innermost first, so the slots above the last one held are released as they go.
    static ENTERED: RefCell<Vec<Option<CallbackScopeGuard>>> = const { RefCell::new(Vec::new()) };
}

/// Hold `guard` until the entry made for it drops. A guard for a scope this thread holds already
/// keeps nothing open, so its entry holds no slot.
#[cfg(feature = "model")]
fn hold(guard: CallbackScopeGuard) -> ScopeEntry {
    if !guard.opens() {
        return ScopeEntry::held();
    }
    let slot = ENTERED.with_borrow_mut(|entered| {
        entered.push(Some(guard));
        entered.len() - 1
    });
    ScopeEntry::opened(leave, slot)
}

/// Drop the guard in `slot`, outside the borrow: dropping it leaves the scope it entered.
#[cfg(feature = "model")]
fn leave(slot: usize) {
    let guard = ENTERED.with_borrow_mut(|entered| {
        let guard = entered.get_mut(slot).and_then(Option::take);
        while entered.last().is_some_and(Option::is_none) {
            entered.pop();
        }
        guard
    });
    drop(guard);
}

#[cfg(feature = "model")]
impl ComponentScope for CallbackBinding {
    fn enter(&self) -> Result<ScopeEntry, WalkError> {
        CallbackBinding::enter(self).map(hold).map_err(walk_error)
    }

    fn enters_same_scope(&self, other: &dyn ComponentScope) -> bool {
        CallbackBinding::enters_same_scope(self, callback_binding_of(other))
    }

    fn same_component(&self, other: &dyn ComponentScope) -> bool {
        CallbackBinding::same_component(self, callback_binding_of(other))
    }

    fn check_open(&self) -> Result<(), WalkError> {
        self.check_resource().map(|_| ()).map_err(walk_error)
    }

    fn note_input(&self) {
        CallbackBinding::note_input(self);
    }
}

/// Read the nearest canonical record owner's document and history stamp.
/// Local writes, remote merges and undo/redo invalidate it, including an
/// edit followed by its inverse. Reading does not open an action or commit.
/// # Errors
/// Rejects missing/inactive nodes, retired owners, borrowed records and
/// exhausted generations; callers must cancel deferred text insertion.
#[cfg(feature = "model")]
pub fn document_edit_stamp(
    bindings: &BoundNodeMap,
    node: NodeId,
    document: &AutoCommit,
) -> Result<DocumentEditStamp, BoundDispatchError> {
    let entry = bindings
        .nodes
        .get(&node)
        .filter(|entry| !entry.inactive)
        .ok_or(BoundDispatchError::MissingNode)?;
    if let Some(binding) = &entry.binding {
        callback_binding_of(&**binding).document_edit_stamp(document)
    } else {
        Ok(DocumentEditStamp::for_document(document))
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
    #[cfg(feature = "model")]
    if let Some(binding) = &admitted.binding {
        let binding = callback_binding_of(&**binding);
        binding.note_input();
        return binding.with_document(outer, |document, root| {
            // SAFETY: admission and lifetime were checked before this dereference;
            // the caller guarantees live topology and exclusive widget access.
            let widget = unsafe { &mut **pointer };
            let result = callback(widget, document, root);
            let _ = binding.drain_owned_tree(widget, document);
            result
        });
    }
    // SAFETY: as above.
    Ok(callback(unsafe { &mut **pointer }, outer, outer_root))
}

/// The bound target of `control`, a control one of the window's components registered.
#[cfg(feature = "model")]
pub(crate) fn bound_target(
    control: &tilo_ui_base::control_domain::ComponentControl,
) -> super::BoundActionTarget {
    let binding = callback_binding_of(&*control.binding).clone();
    super::BoundActionTarget {
        input: control.input,
        address: super::BoundActionAddress {
            instance_id: binding.instance_id,
            address: control.address.clone(),
        },
        binding,
    }
}

/// The control `target` names, as the window's control domain holds a component's.
#[cfg(feature = "model")]
pub(crate) fn component_control(
    target: super::BoundActionTarget,
) -> tilo_ui_base::control_domain::ComponentControl {
    tilo_ui_base::control_domain::ComponentControl {
        input: target.input,
        address: target.address.address,
        binding: std::rc::Rc::new(target.binding),
    }
}
