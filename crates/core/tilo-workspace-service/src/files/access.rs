//! Who may read and write a workspace's files: the one place the workspace's people and grants
//! are asked about them. The mount enforces what this decides for every member, and a member's
//! device is handed the files' bytes only when this lets its person read them.
//!
//! A file change is decided at the node a receiver of the change decides it at
//! ([`fs_protocol::file_action::verb_node`]), and reading at the tree's own records' node, so
//! the mount, the service and every receiver answer the same question.

use collab_types::caps::Access;
use tilo_action_policy::{ActionKind, Answer, Context, TreePath, WorkspacePolicy};

/// The nodes a member's access to `workspace`'s files is decided at: the tree's records, listing
/// them and writing a file.
#[must_use]
pub fn nodes(workspace: &str) -> [TreePath; 3] {
    let file_action = |verb| {
        TreePath::workspace(workspace).joined(&fs_protocol::file_action::verb_node(None, verb))
    };
    [
        collab_types::document_node(
            workspace,
            fs_protocol::tree::FILES_APP,
            &fs_protocol::tree::document_id(),
        ),
        file_action(fs_protocol::tree::LIST),
        file_action(fs_protocol::file_action::verbs::WRITE),
    ]
}

/// What `member` may do to `workspace`'s files under `policy` at `context`'s time: write them,
/// only read them, or nothing.
#[must_use]
pub fn decide(
    policy: &WorkspacePolicy,
    workspace: &str,
    member: &str,
    context: &Context<'_>,
) -> Option<Access> {
    let [records, listing, writing] = nodes(workspace);
    let party = policy.party(member);
    let allows = |node: &TreePath, kind| party.decide(node, kind, context).answer == Answer::Allow;
    if !allows(&records, ActionKind::Read) || !allows(&listing, ActionKind::Read) {
        return None;
    }
    Some(
        if allows(&records, ActionKind::Undoable) && allows(&writing, ActionKind::Undoable) {
            Access::Write
        } else {
            Access::Read
        },
    )
}

/// Where a workspace's files appear in the mount: the path a role's access ranges over.
pub const WORKSPACE_FILES: &str = "/var/workspace";

/// The access list the Files mounts enforce, and when it next changes by itself.
pub struct Listed {
    /// Who may read and write which files, in the mounts' terms.
    pub authority: collab_types::caps::FsAuthority,
    /// The earliest instant (Unix milliseconds) a member's time-limited grant lapses at: the list
    /// is to be made again then.
    pub refresh_at: Option<u64>,
}

/// The file access of `workspaces`, as the Files mounts of this device enforce it: the grants
/// apps hold through portals in each workspace's sandbox, and what each member may do to the
/// workspace's files as its policy decides it ([`decide`]). Role access is scoped to the
/// workspace's own directory within the shared mount.
///
/// A grant to `person` is listed twice: once by the person's key and once by `device`'s, which
/// is the identity the mount itself authenticates as, so translation can never widen a grant and
/// a revoked one takes both away.
#[must_use]
pub fn list<'a>(
    workspaces: impl IntoIterator<Item = &'a tilo_desktop_model::Workspace>,
    person: &str,
    device: Option<[u8; 32]>,
) -> Listed {
    use collab_types::caps::{CapOverride, Effect, Scope};
    use tilo_action_policy::{Acting, Grantee, Lifetime};

    let acting = person_key(person).map(|key| Acting::person(hex::encode(key)));
    let mut capabilities = Vec::new();
    let context = Context::at_time(tilo_clock::wall());
    let mut next_refresh: Option<u64> = None;
    for workspace in workspaces {
        for capability in workspace.sandbox.iter().filter(|capability| {
            capability.is_durable()
                && matches!(capability.scope, Scope::FsPath(_))
                && matches!(capability.effect, Effect::Grant | Effect::Restrict)
        }) {
            push_with_device(&mut capabilities, capability, acting.as_ref(), device);
        }
        let id = workspace.id().to_string();
        let Some(path) = tilo_paths::workspace_tree(std::path::Path::new(WORKSPACE_FILES), &id)
        else {
            continue;
        };
        let [node, listing, writing] = nodes(&id);
        let refresh_at = workspace
            .policy
            .grants
            .iter()
            .filter_map(|grant| match grant.lifetime {
                Lifetime::Until(at)
                    if at > context.now_unix_ms
                        && [&node, &listing, &writing]
                            .into_iter()
                            .any(|target| context.covers(&grant.node, target)) =>
                {
                    Some(at)
                }
                _ => None,
            })
            .min();
        next_refresh = match (next_refresh, refresh_at) {
            (Some(next), Some(at)) => Some(next.min(at)),
            (next, at) => next.or(at),
        };
        let scope = Scope::FsPath(path.to_string_lossy().into_owned());
        capabilities.push(
            CapOverride::new(
                Grantee::role(tilo_action_policy::Role::Viewer),
                scope.clone(),
                Effect::WorkspaceFileBoundary,
                Access::Read,
            )
            .with_id(format!("workspace-files:{id}"))
            .lasting(Lifetime::UntilRevoked),
        );
        for (member, _) in workspace.policy.people() {
            let Some(access) = decide(&workspace.policy, &id, &member, &context) else {
                continue;
            };
            let capability = CapOverride::new(
                Grantee::person(member.clone()),
                scope.clone(),
                Effect::WorkspaceFileAccess,
                access,
            )
            .with_id(format!("role:{id}:{member}"))
            .lasting(refresh_at.map_or(Lifetime::UntilRevoked, Lifetime::Until));
            push_with_device(&mut capabilities, &capability, acting.as_ref(), device);
        }
    }
    Listed {
        authority: collab_types::caps::FsAuthority { capabilities },
        refresh_at: next_refresh,
    }
}

fn person_key(person: &str) -> Option<[u8; 32]> {
    hex::decode(person).ok()?.try_into().ok()
}

/// List `capability` as it stands, and again bound to this device's key when it reaches the
/// person sitting here: the mount acts as the device, so the person's grants must reach it
/// without widening anything.
fn push_with_device(
    capabilities: &mut Vec<collab_types::caps::CapOverride>,
    capability: &collab_types::caps::CapOverride,
    acting: Option<&tilo_action_policy::Acting>,
    device: Option<[u8; 32]>,
) {
    capabilities.push(capability.clone());
    let (Some(device), Some(acting)) = (device, acting) else {
        return;
    };
    if !capability.reaches(acting) {
        return;
    }
    let mut bound = capability.clone();
    bound.grantee = tilo_action_policy::Grantee::Member(tilo_action_policy::MemberId::person(
        hex::encode(device),
    ));
    capabilities.push(bound);
}

impl crate::workspaces::Workspaces {
    /// [`list`] for the workspaces this service holds: each one's policy as the service reads it,
    /// and the grants apps hold in it as its shared desktop record keeps them.
    pub async fn files_access(&self, person: &str, device: Option<[u8; 32]>) -> Listed {
        let mut held = Vec::new();
        for id in self.held_workspaces() {
            let Some(parsed) = tilo_desktop_model::WorkspaceId::parse(&id) else {
                continue;
            };
            let Some(policy) = self.policy_of(&id).await else {
                continue;
            };
            let mut workspace = self
                .shared_layout(&id, parsed)
                .await
                .unwrap_or_else(|| tilo_desktop_model::Workspace::new(parsed, ""));
            workspace.policy = policy;
            held.push(workspace);
        }
        list(&held, person, device)
    }

    /// `workspace` as its shared desktop record lays it out, when this device reads one.
    async fn shared_layout(
        &self,
        workspace: &str,
        id: tilo_desktop_model::WorkspaceId,
    ) -> Option<tilo_desktop_model::Workspace> {
        use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
        let read = self
            .own_read(
                workspace,
                (DESKTOP_APP, DESKTOP_DOCUMENT),
                &[],
                LAYOUT_BYTES,
            )
            .await
            .ok()?;
        let crate::workspace_documents::DocumentRead::Whole { bytes, .. } = read else {
            return None;
        };
        let doc = automerge::AutoCommit::load(&bytes).ok()?;
        tilo_desktop_model::shared_resource::device::layout_of(&doc, id)
    }
}

/// The most bytes of a workspace's shared desktop record read for its grants.
const LAYOUT_BYTES: usize = 64 * 1024 * 1024;

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
    use collab_types::caps::FsAuthority;
    use collab_types::caps::{Access, CapOverride, Effect, Scope};
    use tilo_action_policy::{Acting, Answer, Grantee, Lifetime, Role};
    use tilo_desktop_model::{Layout, Workspace};

    /// The list the mounts of the device `device` are told for `tree`'s workspaces, as `person`.
    fn listed(tree: &Layout, person: &str, device: Option<[u8; 32]>) -> FsAuthority {
        list(&tree.workspaces, person, device).authority
    }

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

    /// Does the list the mounts are told let `acting` read `path` ? This is the decision every
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
        fn the_list_the_mounts_are_told_grants_what_the_workspace_grants_now;

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
            let read_only = listed(&tree, PERSON, None);
            assert!(
                may_read(&read_only, &member, &note),
                "a member reads the workspace's files"
            );
            assert!(
                !may_write(&read_only, &member, &note),
                "a member with a read-only role cannot write them"
            );

            tree.workspaces[0].admit(MEMBER, Role::Editor);
            let editing = listed(&tree, PERSON, None);
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
            let overridden = listed(&tree, PERSON, None);
            assert!(!may_read(&overridden, &member, &note));
            assert!(!may_read(&overridden, &member, &other_note));
            assert!(may_read(&overridden, &member, "/var/workspace/personal.txt"));
            tree.workspaces[0].sandbox.pop();

            tree.workspaces[0].policy.members.remove(MEMBER);
            let removed = listed(&tree, PERSON, None);
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
            let listed = listed(&tree, PERSON, None);
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
            let published = listed(&tree, PERSON, device);
            assert!(may_read(&published, &actor, &format!("{FOLDER}/plan.md")));
            // The device key the mount authenticates as holds the same grant.
            assert!(may_read(
                &published,
                &Acting::person(hex::encode([9u8; 32])),
                &format!("{FOLDER}/plan.md")
            ));

            // The person takes the folder back while an app has it open.
            assert!(collab_types::caps::sandbox::revoke(&mut tree.workspaces[0].sandbox, "brief"));
            let now = listed(&tree, PERSON, device);
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
            let published = listed(&tree, PERSON, None);
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
