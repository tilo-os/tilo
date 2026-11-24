//! What the host answers itself in a workspace through the device's dispatch it answers for
//! whoever the asking device's certificate acts as.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    WORKSPACE_GOVERNANCE_PROVIDER,
};

use crate::workspaces::fixtures::{governor, test_host};

/// The person on her own device lists the members; a caller on that same device acting as
/// anyone else (a bot, an agent the device handed its requests to) is not answered as her.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hosts_own_answers_go_to_whoever_the_device_acts_as() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let host = test_host(dir.path(), &key, &owner).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    // Nothing before the host's own answer turns the handed agent away.
    workspaces.accept_agents_from_other_devices(true);
    let members = || InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(
            &workspace,
            crate::workspaces::actions::MEMBERS,
        ),
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let asked = |caller: Caller, request: InvokeRequest| {
        let dispatch = workspaces.device_actions();
        async move {
            dispatch
                .invoke(&caller, request)
                .await
                .expect("the device's dispatch answers")
                .map(|receipt| receipt.status)
        }
    };
    let (person, device) = (owner.user_public_key, owner.device_public_key);
    let hers = Caller::authenticated(person, device).in_workspace(&workspace);
    assert!(
        matches!(
            asked(hers, members()).await,
            Ok(ReceiptStatus::Completed { .. })
        ),
        "the person on her own device lists the members"
    );
    assert_eq!(members().target.provider, WORKSPACE_GOVERNANCE_PROVIDER);
    let bot = Caller::bot("so.tilo.bots.reminder", person, device).in_workspace(&workspace);
    let handed = Caller::authenticated(person, device)
        .delegated_to(tilo_actions::remote::device_agent_id(&person, "editor"))
        .named("editor")
        .in_agent_session("session-1".to_owned())
        .in_workspace(&workspace);
    for (who, caller) in [("a bot", bot), ("an agent her device handed to", handed)] {
        assert_eq!(
            asked(caller, members()).await,
            Err(ActionCallError::NotPermitted),
            "{who} on her device is not answered as her"
        );
    }
}
