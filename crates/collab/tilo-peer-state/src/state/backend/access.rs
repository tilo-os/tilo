//! Shared current authority decisions.
use super::SystemTime;
use tilo_action_policy::{ActionKind, Answer, Authority};

/// Whether `authority` may read the record `record` of `app` in `workspace`.
pub(in crate::state) fn document_read(
    authority: &Authority<'_>,
    workspace: &str,
    (app, record): (&str, &str),
    own: Option<&collab_types::OwnResource>,
    now: SystemTime,
) -> bool {
    // A member's own record is read by its person alone.
    let person = own.map(collab_types::OwnResource::person);
    let mut cx = tilo_action_policy::Context::at_time(now);
    if let Some(person) = &person {
        cx = cx.with_own_resource(record, person);
    }
    authority
        .decide(
            &collab_types::document_node(workspace, app, record),
            ActionKind::Read,
            &cx,
        )
        .answer
        == Answer::Allow
}
