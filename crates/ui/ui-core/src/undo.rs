//! Seeing and driving undo from inside a widget. Undo itself lives on
//! [`State`](crate::state::State) / [`UndoLog`](collab_types::history::UndoLog), and the host
//! ([`App`](crate::app::App)) drives it from Ctrl+Z.

pub use tilo_ui_base::{widget::undo_access::CommittedActions, widget::undo_access::LocalUndo, widget::undo_access::LocalUndoStatus, widget::undo_access::UndoAccess, widget::undo_access::UndoControl, widget::undo_access::UndoRequest, widget::undo_access::UndoStatus, widget::undo_access::handle_local_undo_request, widget::undo_access::keeps_undo_step_open, widget::undo_access::local_undo_status, widget::undo_access::set_undo_status, widget::undo_access::shortcut, widget::undo_access::take_committed_action, widget::undo_access::take_undo_request};

/// Turn the actions a widget dispatched during the last event into exactly one undo
/// boundary. `root` and the `node_map` pointers must point at widgets that are alive for
/// the call — the invariant every `node_map` sweep in [`App`](crate::app::App) already relies
/// on.
#[cfg(feature = "model")]
pub fn record_dispatched_action<S: std::hash::BuildHasher>(
    state: &mut crate::state::State,
    root: *mut dyn tilo_ui_base::widget::Widget,
    node_map: &std::collections::HashMap<taffy::NodeId, *mut dyn tilo_ui_base::widget::Widget, S>,
    author: &tilo_ui_base::permissions::MemberId,
) -> bool {
    let mut brackets: Vec<Vec<automerge::ChangeHash>> = Vec::new();
    let mut visited = std::collections::HashSet::new();
    for w_ptr in std::iter::once(root).chain(node_map.values().copied()) {
        // SAFETY: see the function's safety note; the pointer is live.
        if !visited.insert(unsafe { tilo_ui_base::widget::node_sweep_identity(w_ptr) }) {
            continue;
        }
        // SAFETY: see the function's safety note. The reference is created and
        // dropped inside this iteration, so no two overlap.
        let w = unsafe { &mut *w_ptr };
        if let Some(before) = tilo_ui_base::widget::undo_access::take_committed_action(w) {
            brackets.push(before);
        }
    }
    let Some(before) = earliest_bracket(state, brackets) else {
        return false;
    };
    if state.doc.get_heads() == before {
        return false;
    }
    state.record_committed_action_authored(before, None, None, Some(author.clone()));
    true
}

#[cfg(feature = "model")]
pub(crate) unsafe fn record_dispatched_action_bound(
    state: &mut crate::state::State,
    root: *mut dyn tilo_ui_base::widget::Widget,
    nodes: &std::collections::HashMap<taffy::NodeId, *mut dyn tilo_ui_base::widget::Widget>,
    order: impl IntoIterator<Item = taffy::NodeId>,
    bindings: &tilo_ui_base::widget::bound_nodes::BoundNodeMap,
    author: &tilo_ui_base::permissions::MemberId,
) -> bool {
    let mut brackets = Vec::new();
    {
        // SAFETY: the host provides its live, exclusively owned root. This borrow
        // ends before any descendant pointer is admitted below.
        let root = unsafe { &mut *root };
        if let Ok(_entry) = tilo_ui_base::widget::bound_nodes::enter_widget(root)
            && let Some(before) = tilo_ui_base::widget::undo_access::take_committed_action(root)
        {
            brackets.push(before);
        }
    }
    let mut visited = std::collections::HashSet::new();
    // SAFETY: the root and every node pointer are live for this exclusive sweep.
    visited.insert(unsafe { tilo_ui_base::widget::node_sweep_identity(root) });
    let mut bound_changed = false;
    // A row per item: the sweep visits many widgets of the same component, whose scope is held
    // across them instead of opened and closed for each.
    let mut held = crate::bound_dispatch::HeldScope::default();
    for node in order {
        let Some(&pointer) = nodes.get(&node) else {
            continue;
        };
        // SAFETY: as above; no other borrow of this node is live.
        if !visited.insert(unsafe { tilo_ui_base::widget::node_sweep_identity(pointer) }) {
            continue;
        }
        let binding = bindings
            .binding_for(node)
            .map(|binding| crate::bound_dispatch::callback_binding_of(&*binding).clone());
        // A closed scope is the node's own refusal: the visits below answer nothing for it.
        let _ = held.enter(binding.as_ref());
        if let Some(binding) = binding {
            // Take the bracket first: it needs only the widget, and a sweep
            // finds one on few nodes, if any. Opening the record frame of
            // every bound node to find nothing was most of an idle frame.
            // SAFETY: the stable host topology supplies exclusive node access;
            // admission and lifetime are checked before the widget is reached.
            let taken = unsafe {
                tilo_ui_base::widget::bound_nodes::mutate_bound_node(
                    bindings,
                    nodes,
                    node,
                    crate::bound_dispatch::TakenActionBracket::take,
                )
            };
            if let Ok(Some(taken)) = taken {
                let drained = binding.with_resource_frame(&mut state.doc, false, |doc, _| {
                    binding.drain_taken_actions(taken, doc)
                });
                bound_changed |= matches!(drained, Ok(Ok(true)));
            }
        } else {
            // SAFETY: each unique live node is visited only once this sweep.
            if let Ok(Some(before)) = unsafe {
                tilo_ui_base::widget::bound_nodes::mutate_bound_node(bindings, nodes, node, |widget| {
                    tilo_ui_base::widget::undo_access::take_committed_action(widget)
                })
            } {
                brackets.push(before);
            }
        }
    }
    let Some(before) = earliest_bracket(state, brackets) else {
        return bound_changed;
    };
    if state.doc.get_heads() == before {
        return false;
    }
    state.record_committed_action_authored(before, None, None, Some(author.clone()));
    true
}

#[cfg(feature = "model")]
fn earliest_bracket(
    state: &mut crate::state::State,
    mut brackets: Vec<Vec<automerge::ChangeHash>>,
) -> Option<Vec<automerge::ChangeHash>> {
    match brackets.len() {
        0 => None,
        1 => brackets.pop(),
        _ => brackets
            .into_iter()
            .map(|b| {
                let depth = state.doc.get_changes(&b).len();
                (depth, b)
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, b)| b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_ui_base::widget::undo_access::EDIT_OPERATION;

    #[derive(Default)]
    struct NativeHistoryProbe {
        pending: Option<tilo_actions::InvocationId>,
        acknowledged: bool,
    }

    impl LocalUndo for NativeHistoryProbe {
        fn local_undo_is_deferred(&self) -> bool {
            true
        }
        fn handle_local_undo_request(&mut self, _: UndoRequest) -> bool {
            false
        }
        fn start_local_undo_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            _: UndoRequest,
        ) -> Result<bool, String> {
            if self.pending.is_some() {
                return Err("original operation pending".into());
            }
            self.pending = Some(invocation);
            Ok(true)
        }
        fn poll_local_undo_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            cancellation_requested: bool,
        ) -> Option<tilo_actions::InvocationUpdate> {
            if self.pending != Some(invocation) {
                return None;
            }
            if cancellation_requested {
                self.pending = None;
                return Some(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: None,
                });
            }
            if !self.acknowledged {
                return None;
            }
            self.pending = None;
            Some(tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            })
        }
    }
    impl tilo_ui_base::widget::Widget for NativeHistoryProbe {
        fn name(&self) -> &str {
            "NativeHistoryProbe"
        }
        tilo_ui_base::provides!(dyn LocalUndo);
        crate::testing::inert_widget_methods!(leaf_layout);
    }

    tilo_test_env::scenario! {
        /// A widget's own history answering an undo operation later.
        /// Each phase was a test of its own and builds its own fixture.
        fn deferred_native_history_reports_only_what_the_original_did;

        phase deferred_native_history_waits_for_original_ack_and_preserves_document;
        {
            use tilo_ui_base::{widget::Edits, widget::ResourceOperation, widget::ResourceOperationStart};
            let mut widget = NativeHistoryProbe::default();
            let mut doc = automerge::AutoCommit::new();
            let original = doc.get_heads();
            let invocation = tilo_actions::InvocationId::new();
            let other = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&UndoRequest::Undo).unwrap();
            let operation = ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                EDIT_OPERATION,
                &payload,
            );
            assert!(tilo_ui_base::widget::resource_operation_handlers(&widget).contains(EDIT_OPERATION));
            assert_eq!(
                tilo_ui_base::widget::start_operation(
                    &mut widget,
                    &operation,
                    &mut Edits::lend(&mut doc),
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ResourceOperationStart::Accepted)
            );
            assert!(
                tilo_ui_base::widget::poll_resource_operation(
                    &mut widget,
                    invocation,
                    false,
                    &doc,
                    &automerge::ROOT
                )
                .is_none()
            );
            widget.acknowledged = true;
            assert!(
                tilo_ui_base::widget::poll_resource_operation(
                    &mut widget,
                    other,
                    false,
                    &doc,
                    &automerge::ROOT
                )
                .is_none()
            );
            assert!(
                matches!(tilo_ui_base::widget::poll_resource_operation(&mut widget, invocation, false, &doc, &automerge::ROOT), Some(tilo_actions::InvocationUpdate::Succeeded { invocation: completed, .. }) if completed == invocation)
            );
            assert!(
                tilo_ui_base::widget::poll_resource_operation(
                    &mut widget,
                    invocation,
                    false,
                    &doc,
                    &automerge::ROOT
                )
                .is_none()
            );
            assert_eq!(doc.get_heads(), original);
        }

        phase deferred_native_history_cancellation_does_not_report_success;
        {
            use tilo_ui_base::{widget::Edits, widget::ResourceOperation};
            let mut widget = NativeHistoryProbe::default();
            let mut doc = automerge::AutoCommit::new();
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&UndoRequest::Redo).unwrap();
            let operation = ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                EDIT_OPERATION,
                &payload,
            );
            tilo_ui_base::widget::start_operation(
                &mut widget,
                &operation,
                &mut Edits::lend(&mut doc),
                &automerge::ROOT,
            )
            .unwrap();
            assert!(
                matches!(tilo_ui_base::widget::poll_resource_operation(&mut widget, invocation, true, &doc, &automerge::ROOT), Some(tilo_actions::InvocationUpdate::Cancelled { invocation: canceled, .. }) if canceled == invocation)
            );
            widget.acknowledged = true;
            assert!(
                tilo_ui_base::widget::poll_resource_operation(
                    &mut widget,
                    invocation,
                    false,
                    &doc,
                    &automerge::ROOT
                )
                .is_none()
            );
        }
    }

    fn status(can_undo: bool, can_redo: bool) -> UndoStatus {
        UndoStatus {
            can_undo,
            can_redo,
            undo_summary: can_undo.then(|| "Renamed the card".to_string()),
            redo_summary: can_redo.then(|| "Added a card".to_string()),
        }
    }

    tilo_test_env::scenario! {
        /// What the undo status publishes, admits and describes.
        /// Each phase was a test of its own and builds its own fixture.
        fn undo_status_checks_requests_shortcuts_and_descriptions;

        phase undo_access_publishes_status_and_refuses_unavailable_verbs_once;
        {
            {
                let mut spacer = tilo_ui_base::widget::Spacer::new();
                tilo_ui_base::widget::undo_access::set_undo_status(&mut spacer, &mut || {
                    panic!("default widget must not request a summary")
                });
            }

            {
                use tilo_ui_base::widget::Widget;
                let mut button = crate::widget::UndoButton::undo();
                let mut calls = 0;
                tilo_ui_base::widget::undo_access::set_undo_status(&mut button, &mut || {
                    calls += 1;
                    status(true, false)
                });
                assert_eq!(calls, 1);
                assert!(button.is_enabled());
                assert_eq!(
                    button.accessibility().unwrap().description.as_deref(),
                    Some("Undo: Renamed the card")
                );
            }

            {
                let mut access = UndoAccess::new();
                assert!(!access.request(UndoRequest::Undo), "empty history refuses");
                assert_eq!(
                    access.take_request(),
                    None,
                    "a refused request must not reach the host"
                );

                access.publish(&status(true, false));
                assert!(access.request(UndoRequest::Undo));
                assert_eq!(access.take_request(), Some(UndoRequest::Undo));
                assert!(!access.request(UndoRequest::Redo));
                assert_eq!(access.take_request(), None);
            }

            {
                let mut access = UndoAccess::new();
                access.publish(&status(true, true));
                access.request(UndoRequest::Undo);
                assert_eq!(access.take_request(), Some(UndoRequest::Undo));
                assert_eq!(
                    access.take_request(),
                    None,
                    "a drained request must not fire twice"
                );
            }
        }

        phase undo_shortcuts_and_descriptions_never_promise_a_missing_action;
        {
            {
                use tilo_ui_base::{platform::Platform, platform::PlatformConventions};
                let linux = PlatformConventions::new(Platform::Linux);
                let shortcut = |c, command, shift| shortcut(linux, c, command, shift);
                assert_eq!(shortcut("z", true, false), Some(UndoRequest::Undo));
                assert_eq!(shortcut("Z", true, false), Some(UndoRequest::Undo));
                assert_eq!(shortcut("z", true, true), Some(UndoRequest::Redo));
                assert_eq!(shortcut("Z", true, true), Some(UndoRequest::Redo));
                assert_eq!(shortcut("y", true, false), Some(UndoRequest::Redo));
                // Without Ctrl these are ordinary characters and must reach the widget.
                assert_eq!(shortcut("z", false, false), None);
                assert_eq!(shortcut("Z", false, true), None);
                assert_eq!(shortcut("k", true, false), None);
                // ⌘Y is History on macOS, never Redo.
                let mac = PlatformConventions::new(Platform::MacOs);
                assert_eq!(super::shortcut(mac, "y", true, false), None);
            }

            {
                let empty = UndoStatus::empty();
                assert_eq!(empty.description(UndoRequest::Undo), "Nothing to undo");
                assert_eq!(empty.description(UndoRequest::Redo), "Nothing to redo");

                let live = status(true, true);
                assert_eq!(
                    live.description(UndoRequest::Undo),
                    "Undo: Renamed the card"
                );
                assert_eq!(live.description(UndoRequest::Redo), "Redo: Added a card");
            }

            {
                let s = UndoStatus {
                    can_undo: true,
                    ..Default::default()
                };
                assert_eq!(s.description(UndoRequest::Undo), "Undo");
            }
        }
    }
}

/// What `entry`'s step did, as its action record says it: the step an undo or redo moved
/// is named by itself, not by the change that moved it
/// ([`UndoEntry::step`](crate::state::UndoEntry::step)).
#[cfg(feature = "model")]
pub(crate) fn step_summary(entry: &crate::state::UndoEntry) -> Option<String> {
    entry
        .step
        .as_ref()
        .and_then(crate::history_source::sentence::step_summary)
}

/// What `state`'s history can do for `author` right now, ready to hand to a widget.
#[cfg(feature = "model")]
pub fn state_status(
    state: &crate::state::State,
    author: Option<&tilo_ui_base::permissions::MemberId>,
) -> UndoStatus {
    status_snapshot(&state.doc, state.undo_log(), author)
}

/// The undo status of `history` for `author`, read without finalizing a displayed owner's
/// live edit.
#[cfg(feature = "model")]
pub(crate) fn status_snapshot(
    doc: &automerge::AutoCommit,
    history: &collab_types::history::UndoLog,
    author: Option<&tilo_ui_base::permissions::MemberId>,
) -> UndoStatus {
    let undo = history.next_undo_as(doc, author);
    let redo = history.next_redo_as(doc, author);
    UndoStatus {
        can_undo: undo.is_some(),
        can_redo: redo.is_some(),
        undo_summary: undo.as_ref().and_then(step_summary),
        redo_summary: redo.as_ref().and_then(step_summary),
    }
}
