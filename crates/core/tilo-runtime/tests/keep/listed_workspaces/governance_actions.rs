//! A hosted workspace's administration answers as `tilo.workspace` actions over
//! the device action service, by the same decision as its wire entries: the owner's
//! device invites; a member the workspace grants nothing is not offered the action. The
//! host's own operations answer the same way as `tilo.host` actions, to the server's own
//! person only, and a member's own business as `tilo.member` actions.
use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER,
    WORKSPACE_INVITATIONS, workspace_governance_child, workspace_governance_resource,
    workspace_member,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_peer_state::actions::{ActionService, NoActions, RemoteDevice};
use tilo_server_client::ServerAddress;
use tilo_server_client::link::{Dialer, Purpose};
use tilo_workspace_service::workspaces::{
    HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE, LIST_WORKSPACES,
};

use super::*;

/// `device`'s link to the workspace `workspace_id` on `server`: every member device's way
/// into a workspace it holds.
pub(super) async fn subscribed(
    server: &Server,
    device: &TestDevice,
    workspace_id: WorkspaceId,
) -> Linked {
    Linked::open(server, device, workspace_id)
        .await
        .expect("the workspace admits its own member")
}

/// The host's actions in `workspace`, asked as `device` over the device action service, from
/// the device's own endpoint: a device with no copy of the workspace asks its host this way, and
/// one with a copy asks through its link ([`Linked::asking_in`]).
///
/// The link is a device's session as much as its question: a host answers a request from a
/// device the workspace holds no certificate for by asking that device which certificate it
/// answers under, over this same link. The device answers, as one holding its identity does; a
/// device that kept silent would hear the refusal only once the reply deadline passed.
pub(super) async fn asking(server: &Server, device: &TestDevice, workspace: &str) -> RemoteDevice {
    let endpoint = device_endpoint(device).await;
    let dialer = Dialer::from_endpoint(ServerAddress::at(server.node_addr()), endpoint.clone());
    let link: Arc<dyn tilo_peer_state::transport::Transport> = Arc::new(
        dialer
            .link(Purpose::Actions)
            .await
            .expect("the host answers its actions link"),
    );
    let service = ActionService::new(device.sk.clone(), Arc::new(NoActions));
    service.answer_under(device.cert.clone());
    let answering = service.clone();
    let session = Arc::clone(&link);
    tokio::spawn(async move {
        let execution = tilo_actions::remote::ExecutionLease::mint();
        let _ = answering.serve_transport(&*session, None, &execution).await;
        execution.close();
    });
    service.remote_over(link, host_device(server), workspace)
}

/// The device `server` signs as: what an action addressed to it names.
fn host_device(server: &Server) -> [u8; 32] {
    server
        .workspaces()
        .expect("the server holds workspaces")
        .host_device()
}

/// What the server answers `message`, asked over `actions`, or why it did not.
pub(super) async fn try_ask(
    actions: &RemoteDevice,
    message: RemoteActionMessage,
) -> Result<RemoteActionMessage, String> {
    tilo_test_env::within(
        "the server answers the action",
        actions.ask_message(message),
    )
    .await
    .ok_or_else(|| "the server that answers the action was dropped".to_owned())?
    .map_err(|error| error.to_string())
}

/// What the server answers `message`, sent over `actions`.
pub(super) async fn ask(
    actions: &RemoteDevice,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    try_ask(actions, message)
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

/// What the server produced for the host operation `action` with `argument`, asked of it by
/// `device` over `actions`, read as `T`. The host answers an operation that names no workspace
/// through what the request itself proves, so `device` signs it.
pub(super) async fn host_action<T: serde::de::DeserializeOwned>(
    actions: &RemoteDevice,
    device: &TestDevice,
    action: &str,
    argument: serde_json::Value,
) -> T {
    host_action_in(actions, device, "", action, argument).await
}

/// [`host_action`] for the operation `device` asks inside `workspace`, which its signature
/// names: a device asks its host the operations from within a workspace it holds there.
pub(super) async fn host_action_in<T: serde::de::DeserializeOwned>(
    actions: &RemoteDevice,
    device: &TestDevice,
    workspace: &str,
    action: &str,
    argument: serde_json::Value,
) -> T {
    let RemoteActionMessage::Invoke(mut request) = host_operation(action, argument) else {
        panic!("a host operation is one invocation");
    };
    request.signed = None;
    request.signed = Some(Box::new(
        tilo_server_protocol::invocation::sign_invocation_now(
            workspace,
            &request,
            &device.cert,
            &device.sk,
            tilo_clock::unix_now_ns(),
        )
        .expect("the device signs its own request"),
    ));
    let caller = Caller::authenticated(device.cert.user_public_key, device.device_pubkey());
    let receipt = tilo_test_env::within(
        "the host answers its own operation",
        ActionProvider::invoke(actions, &caller, request),
    )
    .await
    .expect("the host answers its own operation")
    .unwrap_or_else(|error| panic!("the host refused its own operation: {error}"));
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!(
            "the host's own operations complete at once: {:?}",
            receipt.status
        );
    };
    serde_json::from_value(output.value.unwrap_or_default()).unwrap()
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
        .enter(&workspace.workspace, (&laptop.cert, &laptop.sk))
        .await
        .unwrap();
    super::members::admit(&server, &host, &phone, &workspace.workspace).await;

    // Alice's laptop invites through the action: the workspace records it, as
    // the administration entry would have.
    let owner = subscribed(&server, &laptop, workspace.workspace_id).await;
    let owner_actions = owner
        .asking_in(&workspace.workspace)
        .await
        .expect("the host answers its actions link");
    let RemoteActionMessage::Receipt(receipt) =
        ask(&owner_actions, invite(&workspace.workspace)).await
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
        &owner_actions,
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
    let member_actions = member
        .asking_in(&workspace.workspace)
        .await
        .expect("the host answers its actions link");
    let RemoteActionMessage::Receipt(receipt) =
        ask(&member_actions, invite(&workspace.workspace)).await
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
    let home = asking(&server, &laptop, "").await;
    let created: CreatedWorkspace = host_action(
        &home,
        &laptop,
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
        host_action(&home, &laptop, LIST_WORKSPACES, serde_json::json!({})).await;
    assert!(
        listed
            .iter()
            .any(|workspace| workspace.workspace == created.workspace.workspace)
    );
    let entered: EnteredWorkspace = host_action(
        &home,
        &laptop,
        ENTER_WORKSPACE,
        serde_json::json!({ "workspace": created.workspace.workspace }),
    )
    .await;
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
    // told the workspaces he is in and runs none of the host's own operations.
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
    let member_actions = member
        .asking_in(&created.workspace.workspace)
        .await
        .expect("the host answers its actions link");
    // The listing answers a member the workspaces of theirs the host holds, and
    // no other: it names the workspace Bob was admitted to, as the role he holds.
    let listed: Vec<WorkspaceListing> = serde_json::from_value(
        outcome(
            ask(
                &member_actions,
                host_operation(LIST_WORKSPACES, serde_json::json!({})),
            )
            .await,
        )
        .expect("a member is listed the workspaces they are in"),
    )
    .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|workspace| (workspace.workspace.as_str(), workspace.role.as_deref()))
            .collect::<Vec<_>>(),
        [(created.workspace.workspace.as_str(), Some("editor"))],
        "a member is listed the workspace they were admitted to, and it alone"
    );
    // Making a workspace here is the host's person's, and a member's ask for one runs nothing.
    assert!(
        outcome(
            ask(
                &member_actions,
                host_operation(CREATE_WORKSPACE, serde_json::json!({ "name": "Bob's" })),
            )
            .await,
        )
        .is_err(),
        "workspace.create ran for a member"
    );
    // Entering is no host operation: any device asks for the workspace it holds there, and is
    // answered the invitation that is its own, at the role its person holds, never the owner's.
    let entered: EnteredWorkspace = host_action_in(
        &member_actions,
        &phone,
        &created.workspace.workspace,
        ENTER_WORKSPACE,
        serde_json::json!({ "workspace": created.workspace.workspace }),
    )
    .await;
    let own = entered.invitation.decode().unwrap();
    let verified = decode_and_verify_invitation(
        &own.signed_invitation,
        Some(bob.user_pk),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .expect("the member's own invitation is its person's");
    assert_eq!(
        verified.grant().role,
        Role::Editor,
        "a member enters at the role they hold, never above it"
    );
    let RemoteActionMessage::CatalogueResponse { result, .. } = ask(
        &member_actions,
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
    let rows = result.unwrap().rows;
    assert_eq!(
        rows.iter()
            .filter_map(|row| row
                .action
                .as_ref()
                .map(|action| action.address.action.clone()))
            .collect::<Vec<_>>(),
        [LIST_WORKSPACES],
        "a member is offered the listing, and none of the host's own operations"
    );
    assert_eq!(server.workspaces().unwrap().list().await.len(), 1);
    drop(member);
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
        .enter(&workspace.workspace, (&laptop.cert, &laptop.sk))
        .await
        .unwrap();
    super::members::admit(&server, &host, &phone, &workspace.workspace).await;
    let phone_link = subscribed(&server, &phone, workspace.workspace_id).await;

    // Removing is published on the person it removes, and answers the people left.
    let owner = subscribed(&server, &laptop, workspace.workspace_id).await;
    let owner_actions = owner
        .asking_in(&workspace.workspace)
        .await
        .expect("the host answers its actions link");
    let bob_hex = hex::encode(bob.user_pk);
    let removed = outcome(
        ask(
            &owner_actions,
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
    assert!(
        phone_link
            .reopen(&server, workspace.workspace_id)
            .await
            .is_err(),
        "a removed member's next link is refused"
    );
    drop(owner);
    server.shutdown().await;
}
