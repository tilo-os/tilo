//! A governed workspace's administration answers as `tilo.workspace` actions over
//! the server's own remote-action envelope, by the same decision as its wire
//! entries: the owner's device invites; a member the workspace grants nothing is not
//! offered the action. The host's own operations answer the same way as
//! `tilo.host` actions, to the server's own person only, and a member's own
//! business as `tilo.member` actions.
use tilo_actions::remote::{
    ActionCallError, CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER,
    WORKSPACE_INVITATIONS, workspace_governance_child, workspace_governance_resource,
    workspace_member,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_server::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_server::workspaces::{HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE, LIST_WORKSPACES};

use super::*;
use tilo_server::WorkspaceId;

/// A session of `device` with the server, subscribed to nothing.
pub(super) async fn connected(server: &Server, device: &TestDevice) -> ServerClient {
    ServerClient::connect_to_pinned_host(
        device_endpoint(device).await,
        server.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap()
}

/// A session of `device` subscribed to `workspace_id`.
pub(super) async fn subscribed(
    server: &Server,
    device: &TestDevice,
    workspace_id: WorkspaceId,
) -> ServerClient {
    let client = connected(server, device).await;
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("the workspace admits the device", async {
        while !matches!(
            client.recv().await.expect("the session stays open"),
            Envelope::Ready { .. }
        ) {}
    })
    .await;
    client
}

/// Whether `device` is admitted to `workspace_id`: it connects and its
/// subscription is ready.
pub(super) async fn admitted(
    server: &Server,
    device: &TestDevice,
    workspace_id: WorkspaceId,
) -> bool {
    let Ok(client) = ServerClient::connect_to_pinned_host(
        device_endpoint(device).await,
        server.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    else {
        return false;
    };
    if client.send(Envelope::Subscribe { workspace_id }).is_err() {
        return false;
    }
    tilo_test_env::within("the server answers the subscription", async {
        loop {
            match client.recv().await {
                Some(Envelope::Ready { .. }) => break true,
                Some(Envelope::NotAuthorised { .. }) | None => break false,
                Some(_) => {}
            }
        }
    })
    .await
}

/// What the server answers `message`, sent by `client` to the server's own
/// device `host` about `workspace_id`, or why it did not.
pub(super) async fn try_ask(
    client: &ServerClient,
    host: &TestDevice,
    workspace_id: WorkspaceId,
    message: RemoteActionMessage,
) -> Result<RemoteActionMessage, String> {
    client
        .send(Envelope::RemoteAction {
            workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: host.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(message).encode().unwrap(),
        })
        .map_err(|_| "the session closed".to_owned())?;
    tilo_test_env::within("the server answers the action", async {
        loop {
            match client.recv().await {
                Some(Envelope::RemoteAction { frame, .. }) => {
                    break Ok(RemoteActionFrame::decode(&frame).unwrap().message);
                }
                Some(Envelope::RemoteActionUndeliverable { refusal, .. }) => {
                    break Err(refusal.to_string());
                }
                None => break Err("the session closed".to_owned()),
                Some(_) => {}
            }
        }
    })
    .await
}

/// What the server answers `message`, one of its own operations, asked of it
/// directly by `client`.
pub(super) async fn ask_host_of(
    client: &ServerClient,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    client
        .send(Envelope::HostAction {
            frame: RemoteActionFrame::new(message).encode().unwrap(),
        })
        .unwrap();
    tilo_test_env::within("the server answers its own operation", async {
        loop {
            if let Envelope::HostAction { frame } =
                client.recv().await.expect("the session stays open")
            {
                break RemoteActionFrame::decode(&frame).unwrap().message;
            }
        }
    })
    .await
}

/// What the server answers `message`, sent by `client` to the server's own
/// device `host` about `workspace_id`.
pub(super) async fn ask(
    client: &ServerClient,
    host: &TestDevice,
    workspace_id: WorkspaceId,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    try_ask(client, host, workspace_id, message)
        .await
        .unwrap_or_else(|reason| panic!("the server did not answer: {reason}"))
}

pub(super) fn invocation(
    provider: &str,
    resource: String,
    action: &str,
    argument: serde_json::Value,
) -> RemoteActionMessage {
    let serde_json::Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.into(),
            address: ActionAddress::new(resource, action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    })
}

pub(super) fn host_operation(action: &str, argument: serde_json::Value) -> RemoteActionMessage {
    invocation(
        HOST_ACTIONS_PROVIDER,
        HOST_WORKSPACES_RESOURCE.into(),
        action,
        argument,
    )
}

/// What an invocation came to: what it produced, or why the server refused.
#[expect(
    clippy::panic_in_result_fn,
    reason = "a test's helper: an answer that is no finished receipt fails the test"
)]
pub(super) fn outcome(answer: RemoteActionMessage) -> Result<serde_json::Value, ActionCallError> {
    let RemoteActionMessage::Receipt(receipt) = answer else {
        panic!("an invocation is answered with a receipt");
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("the server's own actions complete at once: {other:?}"),
    }
}

/// What the server produced for the host operation `action` with
/// `argument`, asked of it by `client`, read as `T`.
pub(super) async fn host_action<T: serde::de::DeserializeOwned>(
    client: &ServerClient,
    action: &str,
    argument: serde_json::Value,
) -> T {
    let answer = ask_host_of(client, host_operation(action, argument)).await;
    serde_json::from_value(outcome(answer).unwrap()).unwrap()
}

fn invite(workspace: &str) -> RemoteActionMessage {
    let mut argument = serde_json::Map::new();
    argument.insert("role".into(), "editor".into());
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, "invite"),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owner_invites_through_the_workspaces_action_and_an_editor_cannot() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let workspaces = server.workspaces().unwrap();
    let workspace = workspaces.create("Design review").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &laptop.cert)
        .await
        .unwrap();
    super::members::admit(&server, &host, &phone, &workspace.workspace).await;

    // Alice's laptop invites through the action: the workspace records it, as
    // the administration entry would have.
    let owner = subscribed(&server, &laptop, workspace.workspace_id).await;
    let RemoteActionMessage::Receipt(receipt) = ask(
        &owner,
        &host,
        workspace.workspace_id,
        invite(&workspace.workspace),
    )
    .await
    else {
        panic!("an invocation is answered with a receipt");
    };
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the owner invites: {:?}", receipt.status);
    };
    let invite_id = output.value.unwrap()["invite"].as_str().unwrap().to_owned();
    assert!(
        workspaces
            .pending(&workspace.workspace, &host.cert)
            .await
            .unwrap()
            .iter()
            .any(|pending| hex::encode(pending.invite_id) == invite_id)
    );

    // The workspace offers invitations on its invitations resource.
    let RemoteActionMessage::CatalogueResponse { result, .. } = ask(
        &owner,
        &host,
        workspace.workspace_id,
        RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: CatalogueQuery {
                provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
                parent_resource: Some(workspace_governance_resource(&workspace.workspace)),
                ..CatalogueQuery::default()
            },
        },
    )
    .await
    else {
        panic!("a catalogue request is answered with a page");
    };
    let resource = workspace_governance_child(&workspace.workspace, Some(WORKSPACE_INVITATIONS));
    let rows = result.unwrap().rows;
    assert!(
        rows.iter().any(|row| {
            row.resource.id == resource
                && row
                    .action
                    .as_ref()
                    .is_some_and(|action| action.address.action == "invite")
        }),
        "{rows:#?}"
    );

    // Bob, an Editor the workspace grants nothing, is not offered it.
    let member = subscribed(&server, &phone, workspace.workspace_id).await;
    let RemoteActionMessage::Receipt(receipt) = ask(
        &member,
        &host,
        workspace.workspace_id,
        invite(&workspace.workspace),
    )
    .await
    else {
        panic!("an invocation is answered with a receipt");
    };
    assert_eq!(
        receipt.status,
        ReceiptStatus::Rejected {
            error: ActionCallError::UnknownAction
        }
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_servers_person_shares_through_its_host_actions_and_a_member_cannot() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;

    // Before it holds any workspace, Alice's laptop asks her server directly: it
    // creates the workspace with Bob's invitation, finds it listed and enters it,
    // answered the invitation it redeems itself.
    let home = connected(&server, &laptop).await;
    let created: CreatedWorkspace = host_action(
        &home,
        CREATE_WORKSPACE,
        serde_json::json!({
            "name": "Design review",
            "invite_role": "editor",
            "invite_recipient": hex::encode(bob.user_pk),
        }),
    )
    .await;
    assert_eq!(created.workspace.name, "Design review");
    let listed: Vec<WorkspaceListing> =
        host_action(&home, LIST_WORKSPACES, serde_json::json!({})).await;
    assert!(
        listed
            .iter()
            .any(|workspace| workspace.workspace == created.workspace.workspace)
    );
    let entered: EnteredWorkspace = host_action(
        &home,
        ENTER_WORKSPACE,
        serde_json::json!({ "workspace": created.workspace.workspace }),
    )
    .await;
    assert!(entered.changed);
    let own = entered.invitation.decode().unwrap();
    decode_and_verify_invitation(
        &own.signed_invitation,
        Some(alice.user_pk),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .expect("the laptop's own invitation is its person's");
    drop(home);

    // Bob redeems the invitation the workspace was created with; a member, he is
    // not offered the host's operations and runs none of them.
    let first = created.invitation.expect("asked for").decode().unwrap();
    let verified = decode_and_verify_invitation(
        &first.signed_invitation,
        Some(bob.user_pk),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    WorkspaceJoinClient::new(device_endpoint(&phone).await)
        .redeem_hint(
            &verified.grant().transport[0],
            first.signed_invitation,
            phone.cert.clone(),
            &phone.sk,
            [7; 32],
        )
        .await
        .unwrap();
    let workspace = created.workspace.workspace_id;
    let member = subscribed(&server, &phone, workspace).await;
    for (action, argument) in [
        (LIST_WORKSPACES, serde_json::json!({})),
        (CREATE_WORKSPACE, serde_json::json!({ "name": "Bob's" })),
        (
            ENTER_WORKSPACE,
            serde_json::json!({ "workspace": created.workspace.workspace }),
        ),
    ] {
        assert_eq!(
            outcome(ask_host_of(&member, host_operation(action, argument)).await),
            Err(ActionCallError::UnknownAction),
            "{action}"
        );
    }
    let RemoteActionMessage::CatalogueResponse { result, .. } = ask_host_of(
        &member,
        RemoteActionMessage::CatalogueRequest {
            correlation: 2,
            query: CatalogueQuery {
                provider: Some(HOST_ACTIONS_PROVIDER.into()),
                ..CatalogueQuery::default()
            },
        },
    )
    .await
    else {
        panic!("a catalogue request is answered with a page");
    };
    assert!(result.unwrap().rows.is_empty());
    assert_eq!(server.workspaces().unwrap().list().await.len(), 1);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owner_removes_a_member_whose_next_session_is_refused() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let workspaces = server.workspaces().unwrap();
    let workspace = workspaces.create("Design review").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &laptop.cert)
        .await
        .unwrap();
    super::members::admit(&server, &host, &phone, &workspace.workspace).await;
    assert!(admitted(&server, &phone, workspace.workspace_id).await);

    // Removing is published on the person it removes, and answers the people left.
    let owner = subscribed(&server, &laptop, workspace.workspace_id).await;
    let bob_hex = hex::encode(bob.user_pk);
    let removed = outcome(
        ask(
            &owner,
            &host,
            workspace.workspace_id,
            invocation(
                WORKSPACE_GOVERNANCE_PROVIDER,
                workspace_member(&workspace.workspace, &bob_hex),
                "remove",
                serde_json::json!({}),
            ),
        )
        .await,
    )
    .unwrap();
    assert!(
        removed
            .as_array()
            .is_some_and(|people| people.iter().all(|row| row["member"]["person"] != bob_hex)),
        "{removed}"
    );
    assert!(!admitted(&server, &phone, workspace.workspace_id).await);
    drop(owner);
    server.shutdown().await;
}
