//! Shared coordinator for framework surfaces retained inside tiles. Visitors never retain widget
//! references across a topology change.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use std::collections::{HashMap, HashSet};
use taffy::{NodeId, TaffyTree};
use tilo_ui_base::{widget::Widget, widget::bound_nodes::BoundNodeMap};
use winit::event::{ElementState, WindowEvent};

pub use crate::focus_navigation::FocusRestrictions;
impl FocusRestrictions {
    pub fn collect(root: &dyn Widget, tree: &TaffyTree<()>) -> Self {
        let mut restrictions = Self {
            denied: HashSet::new(),
        };
        crate::history_mode::capability::visit_framework_history(root, &mut |history, _| {
            history.extend_focus_denials(tree, &mut restrictions.denied);
        });
        restrictions
    }
}

pub fn has_surfaces(root: &dyn Widget) -> bool {
    let mut found = false;
    crate::history_mode::capability::visit_framework_history(root, &mut |_, _| found = true);
    found
}
pub fn owns_node(root: &dyn Widget, node: NodeId, tree: &TaffyTree<()>) -> bool {
    let mut owned = false;
    crate::history_mode::capability::visit_framework_history(root, &mut |history, _| {
        owned |= history.owns_node(node, tree)
    });
    owned
}
/// One node's focus admission, walking every surface: the reference [`FocusRestrictions`] must agree
/// with.
#[cfg(any(test, feature = "test-support"))]
pub fn allows_focus(root: &dyn Widget, node: NodeId, tree: &TaffyTree<()>) -> bool {
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
pub fn observe_event(
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

/// What one walk for the History surfaces in a tree found.
#[derive(Default)]
pub struct SurfacesWalk {
    /// A surface needs a refresh ([`needs_refresh`]).
    pub needed: bool,
    /// The tree holds any History surface.
    pub found: bool,
}

/// [`needs_refresh`], and what else the walk found ([`SurfacesWalk`]).
pub fn surfaces_need_refresh(
    root: &dyn Widget,
    doc: &automerge::AutoCommit,
    fallback_width: f64,
    status: Option<&crate::widget::WindowStatus>,
) -> SurfacesWalk {
    let mut walk = SurfacesWalk::default();
    crate::history_mode::capability::visit_framework_history(root, &mut |history, live| {
        walk.found = true;
        let width = if history.get_rect().width() > 0.0 {
            history.get_rect().width()
        } else {
            fallback_width
        };
        walk.needed |= history.validate_root(live).is_err()
            || !history.sync_status_matches(status)
            || history.needs_refresh(doc, width);
    });
    walk
}
pub fn capture_entries(root: &mut dyn Widget, doc: &automerge::AutoCommit) {
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, live| {
        if history.captures_input() {
            history.capture_embedded_pending_controls(live, doc);
        }
    });
}
#[cfg(any(test, feature = "test-support"))]
pub fn discard_active_output(root: &mut dyn Widget) {
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, live| {
        if history.captures_input() {
            history.discard_pending_output(live);
        }
    });
}
pub struct PendingCommand {
    pub surface: uuid::Uuid,
    pub command: super::pending_controls::ContinuationCommand,
}
pub fn drain_pending_commands(root: &mut dyn Widget) -> Vec<PendingCommand> {
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
pub fn prepare_pending_command(
    root: &mut dyn Widget,
    surface: uuid::Uuid,
    token: &super::pending_controls::ContinuationToken,
) -> Option<super::pending_controls::PreparedContinuation> {
    let mut prepared = None;
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        if history.pending_control_surface_lifetime() == surface {
            prepared = history.prepare_pending_control(token);
        }
    });
    prepared
}
pub fn cancel_pending_command(
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
pub enum PendingCommandOutcome {
    Consumed,
    Refused(String),
}
pub fn finish_pending_command(
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
            PendingCommandOutcome::Refused(reason) => {
                history.pending_control_refused(id, reason.clone());
            }
        }
    });
}
/// Hand `status` to every History surface under `root` that can take it where it already shows
/// ([`super::host::HistoryHost::set_sync_status_in_place`]), so a status-only change repaints that text
/// instead of rebuilding.
pub fn set_sync_status_in_place(
    root: &mut dyn Widget,
    status: Option<&crate::widget::WindowStatus>,
) {
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, live| {
        if !history.sync_status_matches(status) {
            history.set_sync_status_in_place(status, live);
        }
    });
}
/// Called only after the runner has discarded all raw widget maps.
pub fn refresh(
    root: &mut dyn Widget,
    doc: &automerge::AutoCommit,
    fallback_width: f64,
    status: Option<&crate::widget::WindowStatus>,
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
            history.set_sync_status(status.cloned());
        }
        let was_history = history.captures_input();
        history.set_width(width);
        history.present_entries_of(live);
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
/// Run a History API control (the History control in an app's header row) on the History of the tile
/// that hosts the app `owner` names, as the window's own History runs it for a window's app.
pub fn invoke_api(
    root: &mut dyn Widget,
    owner: &HistoryOwner,
    intent: &tilo_actions::ControlInvocation,
) -> Option<Result<(), tilo_actions::ApiError>> {
    let mut result = None;
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, _| {
        if result.is_some() || !history.encloses(owner) {
            return;
        }
        result = Some(run_api_control(history, intent));
    });
    result
}
/// Run one History API control on `history` exactly as its own History control does: resolved against
/// the API this History serves now.
pub(crate) fn run_api_control(
    history: &mut host::HistoryHost,
    intent: &tilo_actions::ControlInvocation,
) -> Result<(), tilo_actions::ApiError> {
    let resources = [history.api_resource()];
    let encoded = crate::bound_dispatch::encode_resource_invocation(
        &resources,
        &intent.address,
        None,
        Some(&intent.arguments),
    )?;
    history
        .invoke_api(&encoded)
        .map_err(tilo_actions::ApiError::Failed)
}
pub fn has_review(root: &dyn Widget) -> bool {
    let mut pending = false;
    crate::history_mode::capability::visit_framework_history(root, &mut |history, _| {
        pending |= history.has_review_request()
    });
    pending
}
/// Settled matching owners are checked again immediately at the shared API.
pub fn service_reviews(
    root: &mut dyn Widget,
    state: &mut crate::State,
    authority: Option<&tilo_ui_base::permissions::Verdicts>,
    author: &tilo_ui_base::permissions::MemberId,
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
///
/// # Safety
///
/// Every widget pointer in `nodes` points at a live widget of the window's
/// current tree that nothing else borrows for this call.
pub unsafe fn move_focus(
    root: &mut dyn Widget,
    target: Option<NodeId>,
    current: Option<NodeId>,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    bindings: &BoundNodeMap,
    tree: &TaffyTree<()>,
) -> bool {
    let mut captured = unsafe {
        tilo_ui_base::widget::layout::CompositionFocus::capture(current, nodes, bindings)
    };
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
                    tilo_ui_base::widget::bound_nodes::inspect_bound_node(
                        bindings,
                        nodes,
                        node,
                        |widget| widget.is_focused(),
                    )
                }
                .unwrap_or(false)
            {
                let _ = unsafe {
                    tilo_ui_base::widget::bound_nodes::mutate_bound_node(
                        bindings,
                        nodes,
                        node,
                        |widget| {
                            widget.set_focus(false);
                        },
                    )
                };
            }
        }
    }
    suspended
}

///
/// # Safety
///
/// Every widget pointer in `nodes` points at a live widget of the window's
/// current tree that nothing else borrows for this call.
pub unsafe fn restore_focus(
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
    saved.and_then(|focus| unsafe { focus.restore(nodes, bindings, None) })
}

/// The runner calls this after its remaining exit layout/draw/focus callbacks. Restoring one surface's
/// focus must not end its ownership halfway through the same return frame, or release any other surface's
/// independent lease. A surface whose return has settled then opens the change a person chose in it;
/// whether any did, so the runner syncs what that asked for.
pub fn finish_returns(root: &mut dyn Widget) -> bool {
    let mut opened = false;
    crate::history_mode::capability::visit_framework_history_mut(root, &mut |history, live| {
        if history.has_settled_control_return() {
            history.discard_pending_output(live);
            history.finish_control_return();
        }
        opened |= history.deliver_opened_entry(live);
    });
    opened
}
