//! The file access list the running shell hands the Files mounts.
//! A Files mount decides every open and read against a capability list, and the session is
//! what keeps that list current: while it runs it tells the mounts the shell's file capabilities
//! on every change, over the workspace trees' socket, so taking a folder away stops the app that
//! holds it at its next open — without closing the app, restarting it, or asking it to notice
//! anything.
//! The list is the workspace's own permissions in the mount's terms
//! ([`tilo_workspace_service::files::access::list`]). It follows the shell's revision, so a role
//! change, a grant or a removed member reaches the mounts in the same pass, never on a timer.

use std::sync::OnceLock;

use collab_types::caps::FsAuthority;
use tilo_desktop_model::Layout;

/// The shell's file capabilities and each workspace's readers, as the Files daemon enforces them
/// for this device; a member's grant that lapses wakes the shell to make the list again then.
#[must_use]
pub(crate) fn list(tree: &Layout, person: &str, device: Option<[u8; 32]>) -> FsAuthority {
    let listed = tilo_workspace_service::files::access::list(&tree.workspaces, person, device);
    if let Some(at) = listed.refresh_at {
        tilo_identity::policy_change::wake_at(at.saturating_mul(1_000_000));
    }
    listed.authority
}

/// This device's key, which is the identity the Files mount acts as.
pub(crate) fn device_key() -> Option<[u8; 32]> {
    static KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();
    *KEY.get_or_init(|| {
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        tilo_device_identity::persist::read_device_key(&paths.device_privkey)
            .ok()
            .map(|key| key.verifying_key().to_bytes())
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]
mod tests {
    use super::*;
    use collab_types::caps::{Access, CapOverride, Effect, Scope};
    use tilo_action_policy::{Acting, Answer, Grantee, Lifetime, Role};
    use tilo_desktop_model::Workspace;
    use tilo_workspace_service::files::access::WORKSPACE_FILES;

    const PERSON: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const FOLDER: &str = "/var/workspace/brief";

    fn tree_with(grant: CapOverride) -> Layout {
        let mut workspace = Workspace::new(tilo_desktop_model::WorkspaceId::new(), "Home");
        workspace.sandbox.push(grant);
        Layout {
            workspaces: vec![workspace],
            ..Layout::default()
        }
    }

    fn app_folder_grant() -> CapOverride {
        CapOverride::grant(
            Grantee::person(PERSON),
            Scope::FsPath(FOLDER.into()),
            Access::Read,
        )
        .through("so.tilo.notes")
    }

    /// Does the list the session published let `acting` read `path` ? This is the decision every
    /// Files open asks ([`collab_types::decide_capability`]), so an answer here is the daemon's
    /// answer.
    fn may_read(list: &FsAuthority, acting: &Acting, path: &str) -> bool {
        collab_types::decide_capability(
            acting,
            &collab_types::ResourceRef::FsPath(path.into()),
            Access::Read,
            &list.capabilities,
            Some(0),
        ) == collab_types::CapDecision::Allow
    }

    /// [`may_read`] for a write.
    fn may_write(list: &FsAuthority, acting: &Acting, path: &str) -> bool {
        collab_types::decide_capability(
            acting,
            &collab_types::ResourceRef::FsPath(path.into()),
            Access::Write,
            &list.capabilities,
            Some(0),
        ) == collab_types::CapDecision::Allow
    }

    tilo_test_env::scenario! {
        fn the_list_the_session_publishes_grants_what_the_workspace_grants_now;

        phase a_members_role_decides_whether_they_write_the_workspace_files;
        {
            const MEMBER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
            let member = Acting::person(MEMBER);
            let mut tree = tree_with(app_folder_grant());
            let note = format!("{WORKSPACE_FILES}/{}/{}/brief/plan.md", tilo_paths::WORKSPACE_TREES_DIR, tree.workspaces[0].id());
            tree.workspaces[0].admit(MEMBER, Role::Viewer);
            let other = Workspace::new(tilo_desktop_model::WorkspaceId::new(), "Other");
            let other_note = format!("{WORKSPACE_FILES}/{}/{}/brief/plan.md", tilo_paths::WORKSPACE_TREES_DIR, other.id());
            tree.workspaces.push(other);
            let read_only = list(&tree, PERSON, None);
            assert!(
                may_read(&read_only, &member, &note),
                "a member reads the workspace's files"
            );
            assert!(
                !may_write(&read_only, &member, &note),
                "a member with a read-only role cannot write them"
            );

            tree.workspaces[0].admit(MEMBER, Role::Editor);
            let editing = list(&tree, PERSON, None);
            assert!(may_write(&editing, &member, &note), "an editor writes them");
            assert_ne!(
                editing.capabilities, read_only.capabilities,
                "a role change is a new list, so the daemon is handed it"
            );

            assert!(!may_read(&editing, &member, &other_note));
            assert!(!may_read(&editing, &member, "/var/workspace/personal.txt"));

            let node = collab_types::document_node(&tree.workspaces[0].id().to_string(), fs_protocol::tree::FILES_APP, &fs_protocol::tree::document_id());
            tree.workspaces[0].policy.grants.push(tilo_action_policy::Grant {
                id: tilo_action_policy::GrantId("files-override".into()),
                grantee: Grantee::person(MEMBER),
                node,
                kinds: tilo_action_policy::KindSet::READ,
                answer: Answer::Deny,
                lifetime: Lifetime::UntilRevoked,
                granted_by: Grantee::person(PERSON),
            });
            let explicit_allow = CapOverride::grant(
                Grantee::person(MEMBER),
                Scope::FsPath(WORKSPACE_FILES.into()),
                Access::Write,
            );
            tree.workspaces[0].sandbox.push(explicit_allow);
            let overridden = list(&tree, PERSON, None);
            assert!(!may_read(&overridden, &member, &note));
            assert!(!may_read(&overridden, &member, &other_note));
            assert!(may_read(&overridden, &member, "/var/workspace/personal.txt"));
            tree.workspaces[0].sandbox.pop();

            tree.workspaces[0].policy.members.remove(MEMBER);
            let removed = list(&tree, PERSON, None);
            assert!(
                !may_read(&removed, &member, &note),
                "a removed member reads nothing"
            );
            assert!(!may_write(&removed, &member, &note));
        }

        phase a_grant_on_the_files_decides_over_the_role;
        {
            const MEMBER: &str = "3333333333333333333333333333333333333333333333333333333333333333";
            let member = Acting::person(MEMBER);
            let mut tree = tree_with(app_folder_grant());
            tree.workspaces[0].admit(MEMBER, Role::Editor);
            let workspace = tree.workspaces[0].policy.workspace.clone();
            let note = format!("{WORKSPACE_FILES}/{}/{workspace}/brief/plan.md", tilo_paths::WORKSPACE_TREES_DIR);
            tree.workspaces[0].policy.grants.push(tilo_action_policy::Grant::new(
                "files-read-only",
                Grantee::person(MEMBER),
                tilo_action_policy::TreePath::workspace(workspace)
                    .joined(&fs_protocol::file_action::tree_node()),
                tilo_action_policy::KindSet::UNDOABLE,
                Answer::Deny,
                Grantee::person(PERSON),
            ));
            let listed = list(&tree, PERSON, None);
            assert!(may_read(&listed, &member, &note), "the role still lets them read");
            assert!(
                !may_write(&listed, &member, &note),
                "a Deny on the files' node takes writing away, as every holder decides it"
            );
        }

        phase a_revoked_folder_is_refused_at_the_next_open_and_the_app_keeps_running;
        {
            let device = Some([9u8; 32]);
            let grant = CapOverride::grant(
                Grantee::person(PERSON),
                Scope::FsPath(FOLDER.into()),
                Access::Read,
            )
            .with_id("brief");
            let keeps = CapOverride::grant(
                Grantee::person(PERSON),
                Scope::FsPath("/var/workspace/notes".into()),
                Access::Read,
            )
            .with_id("notes");
            let mut tree = tree_with(grant);
            tree.workspaces[0].sandbox.push(keeps);
            let actor = Acting::person(PERSON);
            let published = list(&tree, PERSON, device);
            assert!(may_read(&published, &actor, &format!("{FOLDER}/plan.md")));
            // The device key the mount authenticates as holds the same grant.
            assert!(may_read(
                &published,
                &Acting::person(hex::encode([9u8; 32])),
                &format!("{FOLDER}/plan.md")
            ));

            // The person takes the folder back while an app has it open.
            assert!(tree.workspaces[0].revoke_sandbox("brief"));
            let now = list(&tree, PERSON, device);
            assert!(
                !may_read(&now, &actor, &format!("{FOLDER}/plan.md")),
                "the next open is refused"
            );
            assert!(
                may_read(&now, &actor, "/var/workspace/notes/today.md"),
                "everything else the app reaches still works: it is not restarted"
            );
        }

        phase only_durable_file_capabilities_are_published;
        {
            let transient = CapOverride::grant(
                Grantee::person(PERSON),
                Scope::FsPath("/var/workspace/scratch".into()),
                Access::Read,
            )
            .through("so.tilo.notes")
            .lasting(tilo_action_policy::Lifetime::Session);
            let device_only = CapOverride::grant(
                Grantee::person(PERSON),
                Scope::Device(collab_types::caps::DeviceCap::Camera),
                Access::Read,
            )
            .through("so.tilo.notes");
            let mut tree = tree_with(app_folder_grant());
            tree.workspaces[0].sandbox.push(transient);
            tree.workspaces[0].sandbox.push(device_only);
            let published = list(&tree, PERSON, None);
            let ordinary: Vec<_> = published.capabilities.iter()
                .filter(|cap| cap.effect != Effect::WorkspaceFileBoundary)
                .collect();
            assert_eq!(ordinary.len(), 1);
            assert!(matches!(ordinary[0].scope, Scope::FsPath(_)));
            assert_eq!(ordinary[0].effect, Effect::Grant);
            assert_eq!(published.capabilities.len(), 2);
        }
    }
}
