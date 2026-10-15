//! Native UI handoff, run after the pane releases the state writer.
use super::{WorkspaceTreeHandle, person_id_of};
use tilo_desktop::activity::{NativeManagedTaskRequest, NativeManualTaskRequest};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_task::{
    WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskCommandId,
};

pub(crate) fn create(
    tree: &WorkspaceTreeHandle,
    draft: NativeManualTaskRequest,
) -> Result<String, String> {
    let (_, caller) = tilo_identity_host::broker::trusted_current_device(&draft.room)
        .map_err(|error| format!("manual task identity unavailable: {error}"))?;
    commit(tree, draft, caller, std::time::SystemTime::now())
}

pub(crate) fn create_managed(
    tree: &WorkspaceTreeHandle,
    draft: NativeManagedTaskRequest,
) -> Result<String, String> {
    let (_, caller) = tilo_identity_host::broker::trusted_current_device(&draft.person_draft.room)
        .map_err(|error| format!("managed task identity unavailable: {error}"))?;
    commit_managed(tree, draft, caller, std::time::SystemTime::now())
}

fn commit_managed(
    tree: &WorkspaceTreeHandle,
    draft: NativeManagedTaskRequest,
    caller: tilo_user_identity::DeviceCert,
    now: std::time::SystemTime,
) -> Result<String, String> {
    let command = WorkspaceTaskCommand::Queue {
        title: draft.person_draft.title.clone(),
        assigned_agent: draft.assigned_agent,
        route: draft.route,
        resource: draft.person_draft.resource.clone().map(Box::new),
    };
    commit_command(tree, draft.person_draft, caller, now, command)
}

fn commit(
    tree: &WorkspaceTreeHandle,
    draft: NativeManualTaskRequest,
    caller: tilo_user_identity::DeviceCert,
    now: std::time::SystemTime,
) -> Result<String, String> {
    let command = WorkspaceTaskCommand::CreateManual {
        title: draft.title.clone(),
        resource: draft.resource.clone().map(Box::new),
    };
    commit_command(tree, draft, caller, now, command)
}

fn commit_command(
    tree: &WorkspaceTreeHandle,
    draft: NativeManualTaskRequest,
    caller: tilo_user_identity::DeviceCert,
    now: std::time::SystemTime,
    command: WorkspaceTaskCommand,
) -> Result<String, String> {
    let room = super::RoomId::parse(&draft.room).ok_or("invalid manual task room")?;
    let owner = {
        let tree = tree.read();
        if tree.room_of(draft.workspace) != Some(room) || !tree.commits_native_documents(room) {
            return Err("manual task workspace changed".into());
        }
        tree.installed_native_owner()
            .ok_or("native manual task owner unavailable")?
    }; // Owner publication acquires the writer; never retain this read guard.
    if person_id_of(caller.user_public_key).as_deref() != Some(draft.actor.as_str()) {
        return Err("manual task actor differs from the actual device person".into());
    }
    let invocation = uuid::Uuid::new_v4();
    let device = caller.device_public_key;
    let request = OwnerCommitRequest {
        room: draft.room,
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation,
            authority: WorkspaceTaskAuthority::Person { caller },
            command,
        },
    };
    let receipt = owner.commit_workspace_task_command(request.clone(), now)?;
    receipt.validate_for(&request)?;
    let Some(tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id }) =
        owner.workspace_task_result(room, WorkspaceTaskCommandId { device, invocation })?
    else {
        return Err("native manual task receipt missing".into());
    };
    Ok(task_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_ui_queue_projects_only_current_agent_admission_without_a_run() {
        use tilo_action_policy::roles::AgentPreset;
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
        let directory = tempfile::tempdir().unwrap();
        let mut native = super::super::WorkspaceTree::bootstrap_default();
        let room = super::super::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        assert!(native.set_room(native.active, room));
        native.persist_path = Some(directory.path().join("native.json"));
        let tree = native.shared();
        let owner = crate::shell::native_workspace_owner::open(tree.clone()).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[92; 32]);
        let caller = tilo_joined_room::testing::certificate(
            &ed25519_dalek::SigningKey::from_bytes(&[91; 32]),
            &key,
            "Governor",
        );
        let _governor =
            tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &caller).unwrap();
        let now = std::time::SystemTime::now();
        let actor = person_id_of(caller.user_public_key).unwrap();
        let membership = |command| OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: caller.clone(),
                command,
            },
        };
        let enroll = membership(WorkspaceMembershipCommand::AddAgent {
            agent: "scribe".into(),
            preset: AgentPreset::Contributor,
        });
        owner
            .commit_workspace_membership_command(enroll.clone(), &caller, &key, now)
            .unwrap()
            .validate_for(&enroll)
            .unwrap();
        let draft = {
            let writer = tree.write();
            let workspace = writer.active;
            drop(writer);
            NativeManagedTaskRequest {
                person_draft: NativeManualTaskRequest {
                    workspace,
                    room: room.to_string(),
                    title: "Inspect chip evidence".into(),
                    actor: actor.clone(),
                    resource: None,
                },
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            }
        };
        let id = commit_managed(&tree, draft.clone(), caller.clone(), now).unwrap();
        let snapshot = owner.workspace_task_snapshot(room).unwrap();
        assert_eq!(snapshot.tasks[0].id, id);
        assert_eq!(snapshot.tasks[0].created_by, actor);
        assert_eq!(snapshot.tasks[0].assigned_agent.as_deref(), Some("scribe"));
        assert_eq!(
            snapshot.tasks[0].route,
            tilo_desktop_model::WorkspaceTaskRoute::Model
        );
        assert_eq!(
            snapshot.tasks[0].status,
            tilo_desktop_model::WorkspaceTaskStatus::Queued
        );
        assert_eq!(
            tree.read().active_workspace().unwrap().tasks,
            snapshot.tasks
        );
        assert!(
            snapshot
                .controls
                .values()
                .all(|control| control.active_run.is_none() && control.run_epoch == 0)
        );
        let mut forged = draft.clone();
        forged.person_draft.actor = "other-person".into();
        assert!(commit_managed(&tree, forged, caller.clone(), now).is_err());
        let mut missing = draft.clone();
        missing.assigned_agent = "missing-agent".into();
        assert!(commit_managed(&tree, missing, caller.clone(), now).is_err());
        let mut manual = draft.clone();
        manual.route = tilo_desktop_model::WorkspaceTaskRoute::Manual;
        assert!(commit_managed(&tree, manual, caller.clone(), now).is_err());
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), snapshot);
        let removed = membership(WorkspaceMembershipCommand::RemoveAgent {
            agent: "scribe".into(),
        });
        owner
            .commit_workspace_membership_command(removed.clone(), &caller, &key, now)
            .unwrap()
            .validate_for(&removed)
            .unwrap();
        assert!(commit_managed(&tree, draft, caller, now).is_err());
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), snapshot);
    }

    #[test]
    fn native_manual_ui_commit_projects_owner_after_writer_release_and_refuses_actor_mismatch() {
        let (_directory, owner, room, now, _key) =
            crate::shell::native_workspace_owner::test_enrolled_owner();
        let tree = crate::shell::native_workspace_owner::tree_of(&owner);
        let caller = owner
            .with_current(room, |view| Ok(view.device().clone()))
            .unwrap();
        let actor = person_id_of(caller.user_public_key).unwrap();
        let draft = {
            let mut writer = tree.write();
            let workspace = writer.active;
            assert!(
                writer
                    .create_workspace_task(
                        workspace,
                        "Refuse legacy mutation",
                        actor.clone(),
                        None,
                        tilo_desktop_model::WorkspaceTaskRoute::Manual,
                        None,
                    )
                    .is_none()
            );
            drop(writer);
            assert!(
                owner
                    .workspace_task_snapshot(room)
                    .unwrap()
                    .tasks
                    .is_empty()
            );
            NativeManualTaskRequest {
                workspace,
                room: room.to_string(),
                title: "Review chip evidence".into(),
                actor: actor.clone(),
                resource: None,
            }
        };
        let id = commit(&tree, draft.clone(), caller.clone(), now).unwrap();
        let actual = owner.workspace_task_snapshot(room).unwrap();
        assert_eq!(actual.tasks.len(), 1);
        assert_eq!(actual.tasks[0].id, id);
        assert_eq!(actual.tasks[0].created_by, actor);
        assert_eq!(tree.read().active_workspace().unwrap().tasks, actual.tasks);
        assert!(!tree.write().set_workspace_task_status(
            draft.workspace,
            &id,
            tilo_desktop_model::WorkspaceTaskStatus::Done,
            &actor,
        ));
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), actual);
        let mut forged = draft.clone();
        forged.actor = "some-other-person".into();
        assert_eq!(
            commit(&tree, forged, caller.clone(), now).unwrap_err(),
            "manual task actor differs from the actual device person"
        );
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), actual);
        // The public owner service excludes stewarded document publication.
        // Closing that presentation scope must also close the native UI path,
        // even while the actual admitted owner and its task receipt remain.
        tree.write().stewarded_rooms.insert(room);
        assert_eq!(
            commit(&tree, draft, caller, now).unwrap_err(),
            "manual task workspace changed"
        );
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), actual);
    }
}
