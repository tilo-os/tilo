//! A governed room's administration answers as `tilo.workspace` actions over
//! the relay's own remote-action envelope, by the same decision as its wire
//! doors: the owner's device invites; a member the room grants nothing is not
//! offered the action. The host's own operations answer the same way as
//! `tilo.host` actions, to the relay's own person only, and a member's own
//! business as `tilo.member` actions.
use tilo_actions::remote::{
    ActionHostError, CatalogueQuery, InvokeArgument, InvokeRequest, ROOM_GOVERNANCE_PROVIDER,
    ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_relay::rooms::{
    HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, LIST_ROOMS, RoomAdminError, RoomCommand,
};
use tilo_relay::transport::REMOTE_ACTION_ENVELOPE_VERSION;

use super::*;

/// A workspace the relay's own person holds on it before any room: their
/// continuity workspace, as far as the relay can tell.
pub(super) const HOME_WORKSPACE: u64 = 0x686f_6d65;

/// A session of `device` subscribed to `workspace_id`.
pub(super) async fn subscribed(
    relay: &Relay,
    device: &TestDevice,
    workspace_id: u64,
) -> RelayClient {
    let client = RelayClient::connect_to_pinned_host(
        device_endpoint(device).await,
        relay.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("the room admits the device", async {
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
pub(super) async fn admitted(relay: &Relay, device: &TestDevice, workspace_id: u64) -> bool {
    let Ok(client) = RelayClient::connect_to_pinned_host(
        device_endpoint(device).await,
        relay.node_addr(),
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
    tilo_test_env::within("the relay answers the subscription", async {
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

/// What the relay answers `message`, sent by `client` to the relay's own
/// device `host` about `workspace_id`, or why it did not.
pub(super) async fn try_ask(
    client: &RelayClient,
    host: &TestDevice,
    workspace_id: u64,
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
    tilo_test_env::within("the relay answers the action", async {
        loop {
            match client.recv().await {
                Some(Envelope::RemoteAction { frame, .. }) => {
                    break Ok(RemoteActionFrame::decode(&frame).unwrap().message);
                }
                Some(Envelope::RemoteActionUndeliverable { reason, .. }) => break Err(reason),
                None => break Err("the session closed".to_owned()),
                Some(_) => {}
            }
        }
    })
    .await
}

/// What the relay answers `message`, sent by `client` to the relay's own
/// device `host` about `workspace_id`.
pub(super) async fn ask(
    client: &RelayClient,
    host: &TestDevice,
    workspace_id: u64,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    try_ask(client, host, workspace_id, message)
        .await
        .unwrap_or_else(|reason| panic!("the relay did not answer: {reason}"))
}

/// An invocation of `action` on `resource` of `provider` with `argument`.
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
    })
}

/// The host's own operation `action` with `argument`.
pub(super) fn host_operation(action: &str, argument: serde_json::Value) -> RemoteActionMessage {
    invocation(
        HOST_ACTIONS_PROVIDER,
        HOST_ROOMS_RESOURCE.into(),
        action,
        argument,
    )
}

/// What an invocation came to: what it produced, or why the relay refused.
#[expect(
    clippy::panic_in_result_fn,
    reason = "a test's helper: an answer that is no finished receipt fails the test"
)]
pub(super) fn outcome(answer: RemoteActionMessage) -> Result<serde_json::Value, ActionHostError> {
    let RemoteActionMessage::Receipt(receipt) = answer else {
        panic!("an invocation is answered with a receipt");
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("the relay's own actions complete at once: {other:?}"),
    }
}

/// What the relay produced for the host operation `action` with
/// `argument`, asked by `client` on `workspace_id`, read as `T`.
pub(super) async fn host_action<T: serde::de::DeserializeOwned>(
    client: &RelayClient,
    host: &TestDevice,
    workspace_id: u64,
    action: &str,
    argument: serde_json::Value,
) -> T {
    let answer = ask(client, host, workspace_id, host_operation(action, argument)).await;
    serde_json::from_value(outcome(answer).unwrap()).unwrap()
}

fn invite(room: &str) -> RemoteActionMessage {
    let mut argument = serde_json::Map::new();
    argument.insert("role".into(), "editor".into());
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::room_governance(room, "invite"),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owner_invites_through_the_rooms_action_and_an_editor_cannot() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Design review").await.unwrap();
    rooms.enter(&room.room, &laptop.cert).await.unwrap();
    super::topology_namespace::admit(&relay, &host, &phone, &room.room).await;

    // Alice's laptop invites through the action: the room records it, as
    // the administration door would have.
    let owner = subscribed(&relay, &laptop, room.workspace_id).await;
    let RemoteActionMessage::Receipt(receipt) =
        ask(&owner, &host, room.workspace_id, invite(&room.room)).await
    else {
        panic!("an invocation is answered with a receipt");
    };
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the owner invites: {:?}", receipt.status);
    };
    let invite_id = output.value.unwrap()["invite"].as_str().unwrap().to_owned();
    assert!(
        rooms
            .pending(&room.room, &host.cert)
            .await
            .unwrap()
            .iter()
            .any(|pending| hex::encode(pending.invite_id) == invite_id)
    );

    // The room offers its governance on its own resource.
    let RemoteActionMessage::CatalogueResponse { result, .. } = ask(
        &owner,
        &host,
        room.workspace_id,
        RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: CatalogueQuery {
                provider: Some(ROOM_GOVERNANCE_PROVIDER.into()),
                ..CatalogueQuery::default()
            },
        },
    )
    .await
    else {
        panic!("a catalogue request is answered with a page");
    };
    let resource = format!("workspace.permissions:{}", room.room);
    assert!(result.unwrap().rows.iter().any(|row| {
        row.resource.id == resource
            && row
                .action
                .as_ref()
                .is_some_and(|action| action.address.action == "invite")
    }));

    // Bob, an Editor the room grants nothing, is not offered it.
    let member = subscribed(&relay, &phone, room.workspace_id).await;
    let RemoteActionMessage::Receipt(receipt) =
        ask(&member, &host, room.workspace_id, invite(&room.room)).await
    else {
        panic!("an invocation is answered with a receipt");
    };
    assert_eq!(
        receipt.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::UnknownAction
        }
    );
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_relays_person_shares_through_its_host_actions_and_a_member_cannot() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;

    // Before it holds any room, Alice's laptop asks her relay on a workspace
    // it holds there: it creates the room with Bob's invitation, finds it
    // listed and enters it, answered the invitation it redeems itself.
    let home = subscribed(&relay, &laptop, HOME_WORKSPACE).await;
    let created: CreatedRoom = host_action(
        &home,
        &host,
        HOME_WORKSPACE,
        CREATE_ROOM,
        serde_json::json!({
            "name": "Design review",
            "invite_role": "editor",
            "invite_recipient": hex::encode(bob.user_pk),
        }),
    )
    .await;
    assert_eq!(created.room.name, "Design review");
    let listed: Vec<RoomListing> = host_action(
        &home,
        &host,
        HOME_WORKSPACE,
        LIST_ROOMS,
        serde_json::json!({}),
    )
    .await;
    assert!(listed.iter().any(|room| room.room == created.room.room));
    let entered: EnteredRoom = host_action(
        &home,
        &host,
        HOME_WORKSPACE,
        ENTER_ROOM,
        serde_json::json!({ "room": created.room.room }),
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

    // Bob redeems the invitation the room was created with; a member, he is
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
    let workspace = created.room.workspace_id;
    let member = subscribed(&relay, &phone, workspace).await;
    for (action, argument) in [
        (LIST_ROOMS, serde_json::json!({})),
        (CREATE_ROOM, serde_json::json!({ "name": "Bob's" })),
        (ENTER_ROOM, serde_json::json!({ "room": created.room.room })),
    ] {
        assert_eq!(
            outcome(ask(&member, &host, workspace, host_operation(action, argument)).await),
            Err(ActionHostError::UnknownAction),
            "{action}"
        );
    }
    let RemoteActionMessage::CatalogueResponse { result, .. } = ask(
        &member,
        &host,
        workspace,
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
    assert_eq!(relay.rooms().unwrap().list().await.len(), 1);
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_old_administration_door_only_removes_members() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Design review").await.unwrap();
    rooms.enter(&room.room, &laptop.cert).await.unwrap();
    super::topology_namespace::admit(&relay, &host, &phone, &room.room).await;

    // What the actions serve is refused there, naming the action.
    let admin = RoomAdminClient::new(device_endpoint(&laptop).await, laptop.cert.clone());
    for (command, action) in [
        (RoomCommand::List, "`room.list` action of `tilo.host`"),
        (
            RoomCommand::Enter {
                room: room.room.clone(),
            },
            "`room.enter` action of `tilo.host`",
        ),
        (
            RoomCommand::ClaimTopologyNamespace {
                room: room.room.clone(),
                salt: 3,
            },
            "`topology-namespace.claim` action of `tilo.member`",
        ),
    ] {
        let refused = admin.send(relay.node_addr(), command).await.unwrap_err();
        assert!(
            matches!(&refused, RoomAdminError::Refused(reason) if reason.contains(action)),
            "{refused}"
        );
    }

    // Removing a member, which the actions do not run yet, still works.
    let generation = admin
        .remove(
            relay.node_addr(),
            &room.room,
            RemoveMemberRequest {
                request_id: [9; 32],
                user_public_key: bob.user_pk,
            },
        )
        .await
        .unwrap();
    assert!(generation > room.generation);
    assert!(
        rooms
            .summary(&room.room)
            .await
            .unwrap()
            .members
            .iter()
            .any(|member| member.user_public_key == bob.user_pk && member.removed)
    );
    relay.shutdown().await;
}
