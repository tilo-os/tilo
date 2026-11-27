use std::sync::Arc;

use tilo_action_policy::{Answer, Authority, Context, Grantee, Role, TreePath};
use tilo_action_types::ActionKind;
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
        asks: false,
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
        .member_policy()
        .expect("the workspace's policy reads")
}

/// An app's share of one node of the workspace (Chat sharing what a window shows with the
/// members) is the workspace's own member list: the shell asks the daemon, everyone's role over
/// the node is written into the workspace this device keeps, signed as the device, and its end
/// takes it back. Nobody but the device's own person asks it.
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
            .block_on(async move { Box::pin(ask(&workspaces, caller, request)).await })
            .expect("the runtime runs")
    };
    let set = |role| WorkspaceMembershipCommand::Member {
        change: tilo_action_policy::Change::SetScopedRole {
            grantee: Grantee::role(Role::Viewer),
            scope: node.clone(),
            role,
        },
    };
    let shared = |policy: &tilo_action_policy::WorkspacePolicy| {
        policy
            .everyone
            .iter()
            .filter(|entry| entry.scope == node)
            .count()
    };

    let stranger = ShellCaller { person: [7; 32] };
    assert!(
        asked(stranger, set(Some(Role::Viewer))).is_err(),
        "the daemon acts for its own person only"
    );
    assert_eq!(
        shared(&policy(&workspaces, placed)),
        0,
        "a refused share writes nothing"
    );

    asked(person, set(Some(Role::Viewer))).expect("the workspace's owner shares what they hold");
    let policy_now = policy(&workspaces, placed);
    assert_eq!(shared(&policy_now), 1, "the share is the workspace's own");
    assert_eq!(
        Authority::person(policy_now.party_of(Grantee::role(Role::Viewer)))
            .decide(&node, ActionKind::Read, &Context::at_secs(0))
            .answer,
        Answer::Allow,
        "a member reads what is shared"
    );

    asked(person, set(None)).expect("the share ends");
    assert_eq!(
        shared(&policy(&workspaces, placed)),
        0,
        "nothing of the share is left in the workspace"
    );
}
