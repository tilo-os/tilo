//! Who keeps a workspace, as every member of it reads it: the devices the workspace's
//! governance names as holding it, each with its device key. Choosing how many copies it wants
//! is its owner's; reading who keeps it is every member's.
use governance_actions::{ask, asking, outcome};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, InvokeArgument, InvokeRequest, RemoteActionMessage, RemoteActionTarget,
    WORKSPACE_GOVERNANCE_PROVIDER,
};

use super::*;

/// The read of who keeps `workspace`: the verb `copies.set` writes beside, at the address the
/// tree publishes it at (`workspace/servers#keeps`).
fn keeps(workspace: &str) -> RemoteActionMessage {
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, "copies.read"),
        argument: InvokeArgument::Json(serde_json::Map::new()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    })
}

/// A member who is no owner of the workspace reads which devices keep it, and the device
/// keeping it is among them: the server that holds it. The answer comes from the published
/// address the tree gives the read, not from a call only the server may make.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_reads_the_devices_keeping_the_workspace() {
    let fixture = listed_workspace(Role::Editor).await;
    let member = fixture
        .bob_link
        .asking_in(&fixture.workspace.workspace)
        .await
        .expect("the host answers its actions link");
    let RemoteActionMessage::Invoke(read) = keeps(&fixture.workspace.workspace) else {
        panic!("the read is one invocation");
    };
    assert_eq!(
        read.target,
        RemoteActionTarget {
            provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                tilo_actions::remote::workspace_governance_child(
                    &fixture.workspace.workspace,
                    Some(tilo_actions::remote::WORKSPACE_SERVERS),
                ),
                "keeps",
            ),
        },
        "the tree publishes the read beside the write, at servers#keeps"
    );
    let read = outcome(ask(&member, keeps(&fixture.workspace.workspace)).await)
        .expect("a member reads the workspace's copies");
    let server = hex::encode(fixture.host.device_pubkey());
    let named = read["keeps"]
        .as_array()
        .expect("the read names the devices keeping it")
        .iter()
        .filter_map(|keep| keep["device"].as_str())
        .any(|device| device == server);
    assert!(
        named,
        "the server holding the workspace is named among its copies: {read}"
    );
}

/// A further device of a member, one the workspace's signed member list does not name, reads
/// nothing of it: a person is a member, and each of their devices is a way in of its own, which
/// the list has only once the device wrote its row there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_the_member_list_does_not_name_reads_no_copies() {
    let fixture = listed_workspace(Role::Editor).await;
    let unlisted = fixture
        .bob
        .enrol("bob-tablet", DeviceCapabilities::member());
    let actions = asking(&fixture.server, &unlisted, &fixture.workspace.workspace).await;
    let refused = outcome(ask(&actions, keeps(&fixture.workspace.workspace)).await);
    assert!(
        matches!(
            refused,
            Err(ActionCallError::NotPermitted | ActionCallError::Refused { .. })
        ),
        "a device the member list does not name is told nothing of its copies: {refused:?}"
    );
}
