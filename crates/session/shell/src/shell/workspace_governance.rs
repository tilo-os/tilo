//! Governance writes that need this machine's workspace tree: whether it
//! governs the room, and mirroring the change into the room document.

use tilo_desktop::workspace_settings::{
    APP_ACTION_GRANT_ACTION_ID, APP_ACTION_REVOKE_ACTION_ID, PermissionProfileOperation,
};

use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    Actor, CapOverride, Effect, Expiry, Principal, Scope, Subject, Verb,
    WORKSPACE_GOVERNANCE_APP_ID, WorkspaceId, WorkspaceTree,
};

/// The verb a typed app-action grant carries: enough for any action that
/// asks at most to edit. An action that asks to govern is never granted to
/// an application this way.
const APP_ACTION_GRANT_VERB: Verb = Verb::Write;

/// Grant or revoke an application another application's action in
/// `workspace_id`, as `actor`, mirrored into the room document and to
/// disk. `actor` must be allowed the governance action itself, exactly as
/// for permission roles.
///
/// # Errors
///
/// A refusal worded for the person: no authority, a room governed
/// elsewhere, or arguments that name nothing grantable.
pub(crate) fn apply_app_action_operation(
    tree: &mut WorkspaceTree,
    workspace_id: WorkspaceId,
    actor: &Actor,
    operation: &PermissionProfileOperation,
) -> Result<(), String> {
    let (action_id, app, target_app, resource, action) = match operation {
        PermissionProfileOperation::GrantAppAction {
            app,
            target_app,
            resource,
            action,
            ..
        } => (
            APP_ACTION_GRANT_ACTION_ID,
            app,
            target_app,
            resource,
            action,
        ),
        PermissionProfileOperation::RevokeAppAction {
            app,
            target_app,
            resource,
            action,
        } => (
            APP_ACTION_REVOKE_ACTION_ID,
            app,
            target_app,
            resource,
            action,
        ),
        _ => return Err("not an app action grant".into()),
    };
    let (app, target_app, resource, action) = (
        app.trim(),
        target_app.trim(),
        resource.trim(),
        action.trim(),
    );
    if app.is_empty() || target_app.is_empty() || resource.is_empty() || action.is_empty() {
        return Err(
            "name the application, the other application, its resource and the action".into(),
        );
    }
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
    if !workspace.allows(actor, &workspace.governance_action(action_id), Verb::Admin) {
        return Err("the caller is not allowed to change what apps may use".into());
    }
    let scope = Scope::Action {
        app: target_app.to_string(),
        resource: resource.to_string(),
        action: action.to_string(),
    };
    let subject = Subject::app(app);
    if let PermissionProfileOperation::GrantAppAction { lifetime, .. } = operation {
        if target_app == WORKSPACE_GOVERNANCE_APP_ID {
            return Err("governing the workspace is assigned with a permission role".into());
        }
        let expiry = match lifetime.trim().to_ascii_lowercase().as_str() {
            "session" => Expiry::EndOfSession,
            "always" => Expiry::Never,
            _ => return Err("lifetime must be session or always".into()),
        };
        let grantor = actor
            .principals
            .iter()
            .find(|principal| !matches!(principal, Principal::Room(_)))
            .cloned()
            .ok_or_else(|| "the caller carries no identity to grant as".to_string())?;
        let id = workspace.grant_capability(
            CapOverride::grant(subject, scope, APP_ACTION_GRANT_VERB)
                .expiring(expiry)
                .granted_by(grantor),
        );
        let stored = workspace
            .capabilities
            .iter()
            .find(|capability| capability.id == id)
            .cloned();
        if let Some(stored) = stored {
            tree.doc_insert_capability(workspace_id, &stored);
        }
    } else {
        let removed = workspace.revoke_matching(|capability| {
            capability.effect == Effect::Grant
                && capability.subject == subject
                && capability.scope == scope
        });
        for id in removed {
            tree.doc_revoke_capability(workspace_id, &id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{ResourceRef, Workspace};
    use tilo_desktop::workspace_settings::{is_governance_action, permission_profile_resource};

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one grant's whole life: offered, refused to strangers, granted, scoped to its resource, revoked"
    )]
    fn app_action_grants_are_typed_admin_actions_that_only_governors_apply() {
        let mut tree = WorkspaceTree::default();
        let mut ws = Workspace::new(WorkspaceId(93), "Research");
        let owner = "owner".to_string();
        ws.set_owner(owner.clone());
        let room = ws.room.to_string();
        let resource = permission_profile_resource(&ws, &owner);
        for id in [APP_ACTION_GRANT_ACTION_ID, APP_ACTION_REVOKE_ACTION_ID] {
            let action = resource
                .actions
                .iter()
                .find(|action| action.id == id)
                .expect("offered to an administrator");
            assert_eq!(
                action.contract.permission,
                tilo_actions::PermissionRequirement::Admin
            );
            assert_eq!(
                action.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
            assert!(is_governance_action(id));
        }
        let owner_actor = ws.actor_for_person(&owner);
        let stranger = ws.actor_for_person(&"stranger".to_string());
        tree.desktop.workspaces.push(ws);
        let grant = |lifetime: &str, target_app: &str| PermissionProfileOperation::GrantAppAction {
            app: "so.tilo.shortcuts".into(),
            target_app: target_app.into(),
            resource: "agent.transform:freeform".into(),
            action: "freeform".into(),
            lifetime: lifetime.into(),
        };
        let id = WorkspaceId(93);

        assert!(
            apply_app_action_operation(&mut tree, id, &stranger, &grant("always", "so.tilo.agent"))
                .is_err(),
            "someone who does not govern the workspace grants nothing"
        );
        assert!(
            apply_app_action_operation(
                &mut tree,
                id,
                &owner_actor,
                &grant("always", WORKSPACE_GOVERNANCE_APP_ID),
            )
            .is_err(),
            "governing the workspace is not an app action to grant"
        );
        assert!(
            apply_app_action_operation(
                &mut tree,
                id,
                &owner_actor,
                &grant("once", "so.tilo.agent")
            )
            .is_err(),
            "a typed grant lasts a session or until revoked"
        );
        apply_app_action_operation(
            &mut tree,
            id,
            &owner_actor,
            &grant("session", "so.tilo.agent"),
        )
        .expect("the owner grants");
        let as_app = Actor::app("so.tilo.shortcuts").in_room(room);
        let freeform = ResourceRef::Action {
            app: "so.tilo.agent".into(),
            resource: "agent.transform:freeform".into(),
            action: "freeform".into(),
        };
        let ws = &tree.desktop.workspaces[0];
        assert!(ws.allows(&as_app, &freeform, Verb::Write));
        assert!(!ws.allows(&as_app, &freeform, Verb::Admin));
        assert!(
            !ws.allows(
                &as_app,
                &ResourceRef::Action {
                    app: "so.tilo.agent".into(),
                    resource: "agent.transform:summarise".into(),
                    action: "freeform".into(),
                },
                Verb::Write
            ),
            "the grant names one resource's action, not the same action elsewhere"
        );
        let granted = ws
            .capabilities
            .iter()
            .find(|capability| matches!(capability.scope, Scope::Action { .. }))
            .expect("recorded");
        assert_eq!(granted.expiry, Expiry::EndOfSession);
        assert_eq!(granted.granted_by, Some(Principal::Person(owner)));

        apply_app_action_operation(
            &mut tree,
            id,
            &owner_actor,
            &PermissionProfileOperation::RevokeAppAction {
                app: "so.tilo.shortcuts".into(),
                target_app: "so.tilo.agent".into(),
                resource: "agent.transform:freeform".into(),
                action: "freeform".into(),
            },
        )
        .expect("the owner revokes");
        assert!(!tree.desktop.workspaces[0].allows(&as_app, &freeform, Verb::Read));
    }
}
