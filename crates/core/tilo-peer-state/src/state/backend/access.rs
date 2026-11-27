//! Shared current authority decisions.
use super::SystemTime;
use tilo_action_policy::{Answer, Authority};
use tilo_action_types::ActionKind;

/// Whether `authority` may read the record `record` of `app` in `workspace`.
pub(in crate::state) fn document_read(
    authority: &Authority<'_>,
    workspace: &str,
    (app, record): (&str, &str),
    now: SystemTime,
) -> bool {
    authority
        .decide(
            &collab_types::document_node(workspace, app, record),
            ActionKind::Read,
            &tilo_action_policy::Context::at_time(now),
        )
        .answer
        == Answer::Allow
}

/// Whether `authority` may act anywhere within the record `record` of `app`,
/// whose kind is `kind` where the record says so: the record itself, or a part
/// of it one of its grants names. A part or chunk of a chained record is
/// reached through the record it belongs to.
pub(in crate::state) fn document_reached(
    authority: &Authority<'_>,
    workspace: &str,
    (app, record): (&str, &str),
    kind: Option<&str>,
    now: SystemTime,
) -> bool {
    let root = collab_types::document_access_target(app, record).unwrap_or(record);
    let node = match kind {
        Some(kind) if collab_types::sealed_kind(app, root).is_none() => {
            tilo_action_policy::TreePath::workspace(workspace)
                .app(app)
                .kind(kind)
                .resource(root)
        }
        _ => collab_types::document_node(workspace, app, root),
    };
    authority.acts_within(&node, &tilo_action_policy::Context::at_time(now))
}
