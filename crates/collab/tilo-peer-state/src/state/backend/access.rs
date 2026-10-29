//! Shared current authority decisions.
use super::SystemTime;
use tilo_action_policy::{ActionKind, Answer, Authority};

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
