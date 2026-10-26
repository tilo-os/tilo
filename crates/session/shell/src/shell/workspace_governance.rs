//! Governance writes made at this device: checked against the room's
//! policy as this device holds it, then asked of the room's governor, its
//! host. Nothing changes here until the room's admitted document carries
//! the change.

use tilo_desktop::workspace_settings::{GovernanceOperation, apply_governance};

use crate::shell::state::{GovernanceRequest, RoomId, WorkspaceId, WorkspaceTree};

/// Ask the governor of `workspace_id`'s room for a governance `operation`
/// as `person`, after the room's policy as held here allows it: `person`
/// must hold the governance action and may hand out no more than they hold.
///
/// # Errors
///
/// A refusal worded for the person: no authority, or arguments that name
/// nothing grantable.
pub(crate) fn apply_governance_operation(
    tree: &mut WorkspaceTree,
    workspace_id: WorkspaceId,
    person: &str,
    operation: GovernanceOperation,
) -> Result<(), String> {
    let mut edited = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .cloned()
        .ok_or_else(|| "no such workspace".to_string())?;
    apply_governance(
        &mut edited,
        person,
        operation.clone(),
        tilo_clock::unix_now(),
    )?;
    let room = edited.room();
    tree.request_governance(room, governance_request(room, operation)?);
    Ok(())
}

/// The request that asks `room`'s governor for `operation`: a grant or
/// revocation is the room's policy, which the governor decides as it would
/// for a person at its own screen; the rest are desktop commands it applies.
///
/// # Errors
/// The operation names nothing the governor can be asked for.
pub(crate) fn governance_request(
    room: RoomId,
    operation: GovernanceOperation,
) -> Result<GovernanceRequest, String> {
    use GovernanceOperation as Operation;
    use tilo_desktop_model::commands::DesktopCommand;
    let command = match operation {
        // A grant is the room's policy, which the governor decides as it
        // would for a person at its own screen.
        operation @ Operation::Grant { .. } => {
            let grant = governed_grant(room, operation)?;
            return Ok(GovernanceRequest::Policy(
                tilo_action_policy::Change::Grant(grant),
            ));
        }
        Operation::Revoke { grant } => {
            return Ok(GovernanceRequest::Policy(
                tilo_action_policy::Change::Revoke(tilo_action_policy::GrantId::new(grant)),
            ));
        }
        Operation::SavePreset {
            name,
            description,
            rules,
        } => DesktopCommand::SavePreset {
            preset_json: serde_json::to_string(&tilo_action_policy::Preset {
                name,
                description,
                rules,
            })
            .map_err(|error| error.to_string())?,
        },
        Operation::AssignPreset {
            preset,
            subject,
            expires_unix,
        } => {
            let (subject_kind, subject) = match subject {
                tilo_action_policy::Subject::Person(id) => ("person", id),
                tilo_action_policy::Subject::App(id) => ("app", id),
                tilo_action_policy::Subject::Agent(id) => ("agent", id),
                _ => return Err("a grant set is assigned to a person, an app or an agent".into()),
            };
            DesktopCommand::AssignPreset {
                preset,
                subject_kind: subject_kind.into(),
                subject,
                expires_unix,
            }
        }
        Operation::GrantAppAction {
            app,
            target_app,
            resource,
            action,
            lifetime,
        } => DesktopCommand::GrantAppAction {
            app,
            target_app,
            resource,
            action,
            lifetime,
        },
        Operation::RevokeAppAction {
            app,
            target_app,
            resource,
            action,
        } => DesktopCommand::RevokeAppAction {
            app,
            target_app,
            resource,
            action,
        },
    };
    Ok(GovernanceRequest::Command(command))
}

/// The grant a governance `operation` gives in `room`; the governor records
/// who gave it.
fn governed_grant(
    room: RoomId,
    operation: GovernanceOperation,
) -> Result<tilo_action_policy::Grant, String> {
    let GovernanceOperation::Grant {
        subject,
        node,
        kinds,
        answer,
        lifetime,
    } = operation
    else {
        return Err("not a grant".into());
    };
    Ok(tilo_action_policy::Grant::new(
        format!("grant:{}", uuid::Uuid::new_v4()),
        subject,
        tilo_action_policy::TreePath::room(room.to_string()).join(&node),
        kinds,
        answer,
        tilo_action_policy::Subject::Person(String::new()),
    )
    .with_lifetime(lifetime.unwrap_or(tilo_action_policy::Lifetime::UntilRevoked)))
}

/// Say each governance change made here that its room's governor refused,
/// as a failed action is said once its palette row is gone: a warning
/// naming the change, with the governor's reason. What the change showed
/// ahead of the answer is already undone, but for a sandbox capability
/// taken back, which stays taken back
/// ([`WorkspaceTree::refuse_room_change`]). Answers whether any was said.
pub(crate) fn report_governance_refusals(st: &mut crate::shell::chrome_state::ChromeState) -> bool {
    if st.tree().governance_refusals.is_empty() {
        return false;
    }
    let refusals = st.tree_mut().take_governance_refusals();
    for refusal in refusals {
        if let crate::shell::state::RoomChange::Governance(GovernanceRequest::Policy(
            tilo_action_policy::Change::Grant(grant),
        )) = &refusal.change
            && let tilo_action_policy::Subject::Agent(agent) = &grant.subject
        {
            // An agent the door is admitting waits on this answer too.
            st.refused_agent_admissions.push((
                refusal.room.to_string(),
                agent.clone(),
                refusal.reason.clone(),
            ));
        }
        let title = crate::shell::i18n::t(match &refusal.change {
            crate::shell::state::RoomChange::Governance(request) => refused_change_label(request),
            crate::shell::state::RoomChange::Receipt(_) => "governance-change-history",
        });
        st.push_warning_notification(
            crate::shell::i18n::t_args("host-action-cannot-run", &[("title", title.into())]),
            refusal.reason,
        );
    }
    st.bump();
    true
}

/// The string key naming the change `request` asked for.
fn refused_change_label(request: &GovernanceRequest) -> &'static str {
    use tilo_action_policy::Change;
    use tilo_desktop_model::commands::DesktopCommand;
    match request {
        GovernanceRequest::Policy(Change::AddMember { .. } | Change::SetRole { .. }) => {
            "search-action-member-role-set"
        }
        GovernanceRequest::Policy(Change::RemoveMember { .. }) => "search-action-member-remove",
        GovernanceRequest::Policy(
            Change::Ban { .. }
            | Change::Unban { .. }
            | Change::Restrict { .. }
            | Change::Unrestrict { .. },
        ) => "governance-change-members",
        GovernanceRequest::Policy(_)
        | GovernanceRequest::GrantSandbox(_)
        | GovernanceRequest::RevokeSandbox(_)
        | GovernanceRequest::Command(
            DesktopCommand::GrantAppAction { .. } | DesktopCommand::RevokeAppAction { .. },
        ) => "governance-change-access",
        GovernanceRequest::Command(DesktopCommand::RenameWorkspace { .. }) => {
            "global-search-rename-workspace"
        }
        GovernanceRequest::Command(
            DesktopCommand::SavePreset { .. } | DesktopCommand::AssignPreset { .. },
        ) => "governance-change-presets",
        GovernanceRequest::Command(_) => "governance-change-settings",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{WORKSPACE_GOVERNANCE_APP_ID, Workspace};
    use tilo_action_policy::{ActionKind, Answer, Lifetime, Subject};
    use tilo_desktop::workspace_settings::{
        APP_ACTION_GRANT_ACTION_ID, APP_ACTION_REVOKE_ACTION_ID, governance_resource,
        is_governance_action,
    };
    use tilo_desktop_model::commands::DesktopCommand;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one grant's whole life: offered, refused to strangers, granted, scoped to its resource, revoked"
    )]
    fn app_action_grants_are_typed_governance_actions_asked_of_the_governor() {
        let mut tree = WorkspaceTree::default();
        let mut ws = Workspace::new(WorkspaceId(93), "Research");
        let owner = "owner".to_string();
        ws.set_owner(owner.clone());
        let resource = governance_resource(&ws, &owner);
        for id in [APP_ACTION_GRANT_ACTION_ID, APP_ACTION_REVOKE_ACTION_ID] {
            let action = resource
                .actions
                .iter()
                .find(|action| action.id == id)
                .expect("offered to a governor");
            assert_eq!(
                action.contract.kind,
                tilo_action_policy::roles::GOVERNANCE_KIND
            );
            assert_eq!(
                action.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
            assert!(is_governance_action(id));
        }
        tree.desktop.workspaces.push(ws);
        let grant = |lifetime: &str, target_app: &str| GovernanceOperation::GrantAppAction {
            app: "so.tilo.shortcuts".into(),
            target_app: target_app.into(),
            resource: "agent.transform:freeform".into(),
            action: "freeform".into(),
            lifetime: lifetime.into(),
        };
        let id = WorkspaceId(93);

        assert!(
            apply_governance_operation(&mut tree, id, "stranger", grant("always", "so.tilo.agent"))
                .is_err(),
            "someone who does not govern the workspace grants nothing"
        );
        assert!(
            apply_governance_operation(
                &mut tree,
                id,
                &owner,
                grant("always", WORKSPACE_GOVERNANCE_APP_ID),
            )
            .is_err(),
            "governing the workspace is not an app action to grant"
        );
        assert!(
            apply_governance_operation(&mut tree, id, &owner, grant("once", "so.tilo.agent"))
                .is_err(),
            "a typed grant lasts a session or until revoked"
        );
        apply_governance_operation(&mut tree, id, &owner, grant("session", "so.tilo.agent"))
            .expect("the owner grants");
        assert!(
            matches!(
                tree.take_governance_requests().as_slice(),
                [(_, GovernanceRequest::Command(DesktopCommand::GrantAppAction { lifetime, .. }))]
                    if lifetime == "session"
            ),
            "the grant is asked of the room's governor"
        );
        assert!(
            tree.desktop.workspaces[0]
                .policy
                .grants
                .iter()
                .all(|grant| grant.subject != Subject::app("so.tilo.shortcuts")),
            "nothing changes here before the room's admitted document carries it"
        );
        // What the governor applies and the admitted document carries.
        let now = tilo_clock::unix_now();
        apply_governance(
            &mut tree.desktop.workspaces[0],
            &owner,
            grant("session", "so.tilo.agent"),
            now,
        )
        .expect("the governor grants");
        let ws = &tree.desktop.workspaces[0];
        let answer = |resource: &str, kind: ActionKind| {
            ws.subject_authority(Subject::app("so.tilo.shortcuts"))
                .decide(
                    &ws.node()
                        .app("so.tilo.agent")
                        .kind("agent.transform")
                        .resource(resource)
                        .action("freeform"),
                    kind,
                    &tilo_desktop_model::context_at(now),
                )
                .answer
        };
        assert_eq!(
            answer("agent.transform:freeform", ActionKind::Undoable),
            Answer::Allow
        );
        assert_eq!(
            answer("agent.transform:freeform", ActionKind::Outside),
            Answer::Deny,
            "an outside effect is not part of an app-action grant"
        );
        assert_eq!(
            answer("agent.transform:summarise", ActionKind::Undoable),
            Answer::Deny,
            "the grant names one resource's action, not the same action elsewhere"
        );
        let granted = ws
            .policy
            .grants
            .iter()
            .find(|grant| grant.subject == Subject::app("so.tilo.shortcuts"))
            .expect("recorded");
        assert!(matches!(granted.lifetime, Lifetime::Session(_)));
        assert_eq!(granted.granted_by, Subject::person(owner.clone()));

        apply_governance_operation(
            &mut tree,
            id,
            &owner,
            GovernanceOperation::RevokeAppAction {
                app: "so.tilo.shortcuts".into(),
                target_app: "so.tilo.agent".into(),
                resource: "agent.transform:freeform".into(),
                action: "freeform".into(),
            },
        )
        .expect("the owner revokes");
        assert!(matches!(
            tree.take_governance_requests().as_slice(),
            [(
                _,
                GovernanceRequest::Command(DesktopCommand::RevokeAppAction { .. })
            )]
        ));
        apply_governance(
            &mut tree.desktop.workspaces[0],
            &owner,
            GovernanceOperation::RevokeAppAction {
                app: "so.tilo.shortcuts".into(),
                target_app: "so.tilo.agent".into(),
                resource: "agent.transform:freeform".into(),
                action: "freeform".into(),
            },
            now,
        )
        .expect("the governor revokes");
        let ws = &tree.desktop.workspaces[0];
        assert!(
            ws.policy
                .grants
                .iter()
                .all(|grant| grant.subject != Subject::app("so.tilo.shortcuts"))
        );
    }
}
