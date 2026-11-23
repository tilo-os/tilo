use std::sync::Arc;

use tilo_action_policy::share::{ShareAccess, is_of_share};
use tilo_action_policy::{ActionKind, Answer, Authority, Context, Grantee, Role, TreePath};
use tilo_actions::remote::RemoteActionTarget;
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

use super::*;

/// The shell's change `command` to `workspace`'s people and settings, as it asks it.
fn membership(workspace: &str, command: &WorkspaceMembershipCommand) -> InvokeRequest {
    let mut arguments = Map::new();
    arguments.insert(
        "command".to_owned(),
        serde_json::to_value(command).expect("a change encodes"),
    );
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, MEMBERSHIP),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// `workspace`'s policy as this device keeps it now.
fn policy(
    workspaces: &ServiceWorkspaces,
    workspace: WorkspaceId,
) -> tilo_action_policy::WorkspacePolicy {
    workspaces
        .peer(workspace)
        .expect("this device keeps the workspace")
        .governance()
        .governed_policy()
        .expect("the workspace's policy reads")
}

/// An app's share of one node of the workspace (Chat sharing what a window shows with the
/// members) is the workspace's own governance: the shell asks the daemon, the share's grants are
/// written into the workspace this device keeps, signed as the device, and its end takes them
/// back. Nobody but the device's own person asks it.
#[test]
fn an_apps_share_of_a_node_is_the_workspaces_own_grants() {
    let placed = WorkspaceId::new();
    let (workspaces, _directory) = tilo_rt::app()
        .block_on(ServiceWorkspaces::test_hosting(placed))
        .expect("the runtime runs");
    let workspace = placed.to_string();
    let person = ShellCaller {
        person: workspaces.device_cert().user_public_key,
    };
    let node = TreePath::workspace(workspace.clone())
        .app("so.tilo.notes")
        .kind("notes.notebook")
        .resource("plan");
    let asked = |caller: ShellCaller, command: WorkspaceMembershipCommand| {
        let workspaces = Arc::clone(&workspaces);
        let request = membership(&workspace, &command);
        tilo_rt::app()
            .block_on(async move { ask(&workspaces, caller, request).await })
            .expect("the runtime runs")
    };
    let share = WorkspaceMembershipCommand::Share {
        share: "m1".into(),
        node: node.clone(),
        access: ShareAccess::View,
    };

    let stranger = ShellCaller { person: [7; 32] };
    assert!(
        asked(stranger, share.clone()).is_err(),
        "the daemon acts for its own person only"
    );
    assert!(
        policy(&workspaces, placed)
            .grants
            .iter()
            .all(|grant| !is_of_share(grant, "m1")),
        "a refused share writes nothing"
    );

    asked(person, share).expect("the workspace's owner shares what they hold");
    let policy_now = policy(&workspaces, placed);
    let of_share: Vec<_> = policy_now
        .grants
        .iter()
        .filter(|grant| is_of_share(grant, "m1"))
        .collect();
    assert_eq!(
        of_share.len(),
        1,
        "the share is one of the workspace's own grants"
    );
    assert_eq!(
        of_share[0].grantee,
        Grantee::role(Role::Viewer),
        "the grant is the viewer role's, and every role above it reads through it"
    );
    assert_eq!(
        Authority::person(policy_now.party_of(Grantee::role(Role::Viewer)))
            .decide(&node, ActionKind::Read, &Context::at_secs(0))
            .answer,
        Answer::Allow,
        "a member reads what is shared"
    );

    asked(
        person,
        WorkspaceMembershipCommand::Unshare { share: "m1".into() },
    )
    .expect("the share ends");
    assert!(
        policy(&workspaces, placed)
            .grants
            .iter()
            .all(|grant| !is_of_share(grant, "m1")),
        "nothing of the share is left in the workspace"
    );
}
