//! Shared current authority decisions.
use super::{Role, SystemTime};
use tilo_action_policy::{ActionKind, Answer, Authority, Grant, WorkspacePolicy};

/// Whether `authority` may read the record `record` of `app` in `workspace`.
pub(in crate::state) fn document_read(
    authority: &Authority<'_>,
    workspace: &str,
    app: &str,
    record: &str,
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
/// Who a current member is, apart from the workspace's policy their
/// authority is decided under: a [`crate::MemberAuthority`] without its policy, so one
/// policy decides for many members.
pub(in crate::state) struct Member {
    pub role: Role,
    pub person: String,
    pub foreign: bool,
    pub delegate: Option<tilo_action_policy::Bound>,
}
/// The authority of `person` under `policy`, held to `foreign_bound` on a
/// foreign device and to `delegate`'s bound on a delegate's.
pub(in crate::state) fn member_authority<'a>(
    policy: &'a WorkspacePolicy,
    person: &'a str,
    foreign: bool,
    delegate: Option<&'a tilo_action_policy::Bound>,
    foreign_bound: &'a [Grant],
) -> Authority<'a> {
    let authority = policy.person_on_device(person, foreign, foreign_bound);
    match delegate {
        Some(bound) => authority.bounded_by(bound.party(&policy.grants)),
        None => authority,
    }
}
/// Whether `person` may read `app`'s `record` in `workspace` at `now` under
/// `policy`, as [`member_authority`] decides it.
pub(in crate::state) fn check_document_read(
    policy: &WorkspacePolicy,
    person: &str,
    foreign: bool,
    delegate: Option<&tilo_action_policy::Bound>,
    (workspace, app, record): (&str, &str, &str),
    now: SystemTime,
) -> Result<(), String> {
    let bound = tilo_action_policy::roles::foreign_device_bound();
    let authority = member_authority(policy, person, foreign, delegate, &bound);
    if document_read(&authority, workspace, app, record, now) {
        Ok(())
    } else {
        Err(format!("record read forbidden: {app}/{record}"))
    }
}
