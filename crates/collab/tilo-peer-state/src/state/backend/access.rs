//! Shared current authority decisions.
use super::SystemTime;
use tilo_action_policy::{ActionKind, Answer, Authority, TreePath};

/// Whether `authority` may read the record `record` of `app` in `room`.
pub(in crate::state) fn document_read(
    authority: &Authority<'_>,
    room: &str,
    app: &str,
    record: &str,
    now: SystemTime,
) -> bool {
    authority
        .decide(
            &collab_types::document_node(room, app, record),
            ActionKind::Read,
            &tilo_action_policy::Context::at_time(now),
        )
        .answer
        == Answer::Allow
}

/// Whether `authority` may read the room, and change it (`Undoable` on the room
/// itself: its windows, tasks and contents).
pub(in crate::state) fn room(
    authority: &Authority<'_>,
    room: &str,
    now: SystemTime,
) -> (bool, bool) {
    let node = TreePath::room(room);
    let cx = tilo_action_policy::Context::at_time(now);
    let allowed = |kind| authority.decide(&node, kind, &cx).answer == Answer::Allow;
    (allowed(ActionKind::Read), allowed(ActionKind::Undoable))
}
