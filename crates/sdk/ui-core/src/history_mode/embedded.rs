//! Shared coordinator for framework surfaces retained inside tiles. Visitors
//! never retain widget references across a topology change.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::{bound_dispatch::BoundNodeMap, widget::Widget};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use winit::event::{ElementState, WindowEvent};

pub(crate) fn has_surfaces(root: &dyn Widget) -> bool {
    let mut found = false;
    crate::history_mode::capability::visit_framework_history(root, &mut |_, _| found = true);
    found
}
pub(crate) fn owns_node(root: &dyn Widget, node: NodeId, tree: &TaffyTree<()>) -> bool {
    let mut owned = false;
    crate::history_mode::capability::visit_framework_history(root, &mut |history, _| {
        owned |= history.owns_node(node, tree)
    });
    owned
}
pub(crate) fn allows_focus(root: &dyn Widget, node: NodeId, tree: &TaffyTree<()>) -> bool {
    let mut allowed = true;
    crate::history_mode::capability::visit_framework_history(root, &mut |history, _| {
        if (history.captures_input() || history.has_saved_focus())
            && history.composition_contains(node, tree)
            && !history.owns_node(node, tree)
        {
            allowed = false;
        }
    });
    allowed
}
pub(crate) fn observe_event(
    root: &mut dyn Widget,
    event: &WindowEvent,
    scale: f64,
    focused: Option<NodeId>,
    tree: &TaffyTree<()>,
) -> bool {
    let mut controls = false;
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        history.observe_pointer(event, scale);
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } | WindowEvent::Touch(winit::event::Touch {
                phase: winit::event::TouchPhase::Started,
                ..
            })
        ) {
            history.activate_at_pointer(event, scale);
        } else if matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) && let Some(node) = focused
        {
            history.set_keyboard_active(history.composition_contains(node, tree));
        }
        controls |= history.controls_event(event, scale);
    });
    controls
}
pub(crate) fn needs_refresh(
    root: &dyn Widget,
    doc: &automerge::AutoCommit,
    fallback_width: f64,
    status: Option<&str>,
) -> bool {
    let mut needed = false;
    crate::history_mode::capability::visit_framework_history(root, &mut |history, live| {
        let width = if history.get_rect().width() > 0.0 {
            history.get_rect().width()
        } else {
            fallback_width
        };
        needed |= history.validate_root(live).is_err()
            || !history.sync_status_matches(status)
            || history.needs_refresh(doc, width);
    });
    needed
}
pub(crate) fn capture_entries(root: &mut dyn Widget, doc: &automerge::AutoCommit) {
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, live| {
        if history.captures_input() {
            history.capture_embedded_pending_controls(live, doc);
        }
    });
}
pub(crate) fn discard_active_output(root: &mut dyn Widget) {
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, live| {
        if history.captures_input() {
            history.discard_pending_output(live);
        }
    });
}
pub(crate) struct PendingCommand {
    pub(crate) surface: uuid::Uuid,
    pub(crate) command: super::pending_controls::ContinuationCommand,
}
pub(crate) fn drain_pending_commands(root: &mut dyn Widget) -> Vec<PendingCommand> {
    let mut commands = Vec::new();
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        let surface = history.pending_control_surface_lifetime();
        commands.extend(
            history
                .drain_pending_control_commands()
                .into_iter()
                .map(|command| PendingCommand { surface, command }),
        );
    });
    commands
}
pub(crate) fn prepare_pending_command(
    root: &mut dyn Widget,
    surface: uuid::Uuid,
    token: &super::pending_controls::ContinuationToken,
    confirmed: bool,
) -> Option<super::pending_controls::PreparedContinuation> {
    let mut prepared = None;
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        if history.pending_control_surface_lifetime() == surface {
            prepared = history.prepare_pending_control(token, confirmed);
        }
    });
    prepared
}
pub(crate) fn prepare_pending_command_typed(
    root: &mut dyn Widget,
    surface: uuid::Uuid,
    token: &super::pending_controls::ContinuationToken,
    value: &str,
) -> Option<super::pending_controls::PreparedContinuation> {
    let mut prepared = None;
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        if history.pending_control_surface_lifetime() == surface {
            prepared = history.prepare_pending_control_typed(token, value);
        }
    });
    prepared
}
pub(crate) fn cancel_pending_command(
    root: &mut dyn Widget,
    surface: uuid::Uuid,
    token: &super::pending_controls::ContinuationToken,
) {
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        if history.pending_control_surface_lifetime() == surface {
            history.cancel_pending_control(token);
        }
    });
}
pub(crate) enum PendingCommandOutcome {
    Consumed,
    NeedsConfirmation,
    Refused(String),
}
pub(crate) fn finish_pending_command(
    root: &mut dyn Widget,
    surface: uuid::Uuid,
    id: super::pending_controls::PendingControlId,
    outcome: &PendingCommandOutcome,
) {
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        if history.pending_control_surface_lifetime() != surface {
            return;
        }
        match outcome {
            PendingCommandOutcome::Consumed => history.pending_control_consumed(id),
            PendingCommandOutcome::NeedsConfirmation => {
                history.pending_control_needs_confirmation(id);
            }
            PendingCommandOutcome::Refused(reason) => {
                history.pending_control_refused(id, reason.clone());
            }
        }
    });
}
/// Called only after the runner has discarded all raw widget maps.
pub(crate) fn refresh(
    root: &mut dyn Widget,
    doc: &automerge::AutoCommit,
    fallback_width: f64,
    status: Option<&str>,
) {
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, live| {
        if history.validate_root(live).is_err() {
            history.invalidate_owner();
            return;
        }
        let width = if history.get_rect().width() > 0.0 {
            history.get_rect().width()
        } else {
            fallback_width
        };
        if !history.sync_status_matches(status) {
            history.set_sync_status(status.map(str::to_owned));
        }
        let was_history = history.captures_input();
        history.set_width(width);
        history.refresh(doc);
        let returning_live = history.returning_live();
        if was_history || returning_live || history.captures_input() {
            // Only this surface's retained live subtree: other tiles keep
            // their unadmitted local controls and independently running effects.
            history.discard_pending_output(live);
        }
        if returning_live {
            history.finish_return_to_live();
            history.flush_pending_toolbar();
        }
    });
}
pub(crate) fn has_review(root: &dyn Widget) -> bool {
    let mut pending = false;
    crate::history_mode::capability::visit_framework_history(root, &mut |history, _| {
        pending |= history.has_review_request()
    });
    pending
}
/// Settled matching owners are checked again immediately at the shared door.
/// Caller has removed raw maps before any modal replacement.
pub(crate) fn service_reviews(
    root: &mut dyn Widget,
    state: &mut crate::State,
    authority: &crate::permissions::PermissionContext,
    author: &crate::permissions::UserId,
) -> Vec<HistorySourceRegistration> {
    let mut restored = vec![];
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, live| {
        history.service_restore(state, live, authority, author);
        if let Some(source) = history.take_restored_source() {
            restored.push(source);
        }
    });
    restored
}
/// The maps are borrowed only for this call, before any refresh/rebuild.
pub(crate) unsafe fn move_focus(
    root: &mut dyn Widget,
    target: Option<NodeId>,
    current: Option<NodeId>,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    bindings: &BoundNodeMap,
    tree: &TaffyTree<()>,
) -> bool {
    // Capture before borrowing a retained subtree through the mutable visitor.
    let mut captured =
        unsafe { crate::widget::layout::CompositionFocus::capture(current, nodes, bindings) };
    let mut suspended = false;
    let mut resumed = false;
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        let inside = target.is_some_and(|node| history.composition_contains(node, tree));
        history.set_keyboard_active(inside);
        if !history.captures_input()
            && history.has_saved_focus()
            && inside
            && target.is_some_and(|node| !history.owns_node(node, tree))
        {
            history.discard_saved_focus();
            resumed = true;
        }
        if target.is_some_and(|node| history.owns_node(node, tree))
            && current.is_some_and(|node| {
                history.composition_contains(node, tree) && !history.owns_node(node, tree)
            })
        {
            history.preserve_captured_focus(captured.take());
            suspended = true;
        }
    });
    if resumed {
        for &node in nodes.keys() {
            if Some(node) != target
                && unsafe {
                    crate::bound_dispatch::inspect_bound_node(bindings, nodes, node, |widget| {
                        widget.is_focused()
                    })
                }
                .unwrap_or(false)
            {
                let _ = unsafe {
                    crate::bound_dispatch::mutate_bound_node(bindings, nodes, node, |widget| {
                        widget.set_focus(false);
                    })
                };
            }
        }
    }
    suspended
}

pub(crate) unsafe fn restore_focus(
    root: &mut dyn Widget,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    bindings: &BoundNodeMap,
) -> Option<NodeId> {
    let mut saved = None;
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        if history.returning_live() {
            if history.keyboard_is_active() {
                saved = history.take_returning_focus().or_else(|| saved.take());
            } else {
                history.cancel_focus_return();
            }
        }
    });
    // No retained child borrow overlaps raw map access.
    saved.and_then(|focus| unsafe { focus.restore(nodes, bindings, None) })
}

/// The runner calls this after its remaining exit layout/draw/focus callbacks.
/// Restoring one surface's focus must not end its custody halfway through the
/// same return frame, or release any other surface's independent lease.
pub(crate) fn finish_returns(root: &mut dyn Widget) {
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, live| {
        if history.has_settled_control_return() {
            history.discard_pending_output(live);
            history.finish_control_return();
        }
    });
}

pub(crate) fn keyboard_captured(
    root: &dyn Widget,
    focused: Option<NodeId>,
    tree: &TaffyTree<()>,
) -> bool {
    let mut captured = false;
    crate::history_mode::capability::visit_framework_history(root, &mut |history, _| {
        captured |= focused.is_some_and(|node| history.owns_node(node, tree))
            || (history.keyboard_is_active() && history.captures_input());
    });
    captured
}

pub(crate) fn captures_input(root: &dyn Widget) -> bool {
    let mut captured = false;
    crate::history_mode::capability::visit_framework_history(root, &mut |history, _| {
        captured |= history.captures_input()
    });
    captured
}
