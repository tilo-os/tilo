//! Governance writes that need this machine's workspace tree: whether it
//! governs the room, and mirroring the change into the room document.

use tilo_desktop::workspace_settings::{GovernanceOperation, apply_governance};

use crate::shell::state::{WorkspaceId, WorkspaceTree};

/// Apply a governance `operation` to `workspace_id` as `person`, mirrored
/// into the room document and to disk. The room's policy decides: `person`
/// must hold the governance action and may hand out no more than they hold.
///
/// # Errors
///
/// A refusal worded for the person: no authority, a room governed
/// elsewhere, or arguments that name nothing grantable.
pub(crate) fn apply_governance_operation(
    tree: &mut WorkspaceTree,
    workspace_id: WorkspaceId,
    person: &str,
    operation: GovernanceOperation,
) -> Result<(), String> {
    let room = tree
        .room_of(workspace_id)
        .ok_or_else(|| "no such workspace".to_string())?;
    if !room.is_nil() && !tree.governs_room(room) {
        return Err("this device is not the workspace governance authority".into());
    }
    let workspace = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| "no such workspace".to_string())?;
    let applied = apply_governance(workspace, person, operation, tilo_clock::unix_now())?;
    if let Some(preset) = &applied.preset {
        tree.doc_upsert_preset(workspace_id, preset);
    }
    tree.doc_write_policy(workspace_id, &applied.edit);
    Ok(())
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one grant's whole life: offered, refused to strangers, granted, scoped to its resource, revoked"
    )]
    fn app_action_grants_are_typed_governance_actions_that_only_governors_apply() {
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
        let now = tilo_clock::unix_now();
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
        let ws = &tree.desktop.workspaces[0];
        assert!(
            ws.policy
                .grants
                .iter()
                .all(|grant| grant.subject != Subject::app("so.tilo.shortcuts"))
        );
    }
}
